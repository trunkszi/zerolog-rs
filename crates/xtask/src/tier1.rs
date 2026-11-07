#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::SystemTime,
};

const PROBES: [&str; 7] = [
    "probe_level_gate_check",
    "probe_spsc_alloc_push",
    "probe_heap_node_pass",
    "probe_header_format",
    "probe_format_u64",
    "probe_log_u32",
    "probe_merge_pop",
];

/// Throughput ceilings complement the independent assembly division checks.
fn mca_budget(symbol: &str) -> f64 {
    match symbol {
        "probe_format_u64" => 33.0,
        "probe_header_format" => 117.0,
        _ => 4.0,
    }
}

pub struct Artifacts {
    pub ir: PathBuf,
    pub assembly: PathBuf,
    pub object: PathBuf,
    pub rlib: PathBuf,
}

pub fn abi_check() -> i32 {
    dispatch(abi_check_with)
}
pub fn ir_check() -> i32 {
    dispatch(ir_check_with)
}
pub fn asm_check() -> i32 {
    dispatch(asm_check_with)
}
pub fn mca_gate() -> i32 {
    dispatch(mca_gate_with)
}
pub fn size_report() -> i32 {
    dispatch(size_report_with)
}

fn dispatch(check: fn(&Artifacts) -> i32) -> i32 {
    match build_probes() {
        Ok(artifacts) => check(&artifacts),
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}

pub fn build_probes() -> Result<Artifacts, String> {
    let root = workspace_root()?;
    assert_llvm_major()?;
    let target = target_dir(&root);
    let mut cmd = cargo_cmd()?;
    cmd.current_dir(&root)
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_PROFILE_RELEASE_LTO", "off")
        .env("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "1")
        .env("CARGO_TERM_COLOR", "never")
        .args([
            "rustc",
            "--package",
            "zerolog-probes",
            "--release",
            "--lib",
            "--",
            "--emit=llvm-ir,asm,obj",
        ]);
    let output = run(&mut cmd, "cargo rustc probes")?;
    if !output.status.success() {
        return Err(format!("probe build failed\n{}", tail(&output.stderr, &output.stdout)));
    }
    find_artifacts(&target)
}

pub fn abi_check_with(artifacts: &Artifacts) -> i32 {
    let object = artifacts.object.display().to_string();
    let nm = match tool_output("llvm-nm", &["-S", "--size-sort", &object]) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    if let Err(err) = sizes_present(&nm) {
        eprintln!("abi-check: {err}");
        return 1;
    }
    let readobj = match tool_output("llvm-readobj", &["--sections", "--symbols", &object]) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    if !readobj.contains(".text") {
        eprintln!("abi-check: llvm-readobj output has no .text section");
        return 1;
    }
    for probe in PROBES {
        if !readobj.contains(probe) {
            eprintln!("abi-check: llvm-readobj missing {probe}");
            return 1;
        }
    }
    println!("abi-check: probe symbols have nonzero sizes");
    0
}

pub fn ir_check_with(artifacts: &Artifacts) -> i32 {
    let ir = match std::fs::read_to_string(&artifacts.ir) {
        Ok(ir) => ir,
        Err(err) => {
            eprintln!("ir-check: {}", err);
            return 1;
        }
    };
    if let Err(err) = audit_ir(&ir) {
        eprintln!("ir-check: {err}");
        return 1;
    }
    println!("ir-check: register return and hot blocks passed");
    0
}

pub fn asm_check_with(artifacts: &Artifacts) -> i32 {
    let object = artifacts.object.display().to_string();
    let symbols = PROBES.join(",");
    let disassemble = format!("--disassemble-symbols={symbols}");
    let dump = match tool_output("llvm-objdump", &["-dr", "-M", "intel", &disassemble, &object]) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    if let Err(err) = crate::tier2::audit_asm(&dump).and_then(|()| audit_pipeline_asm(&dump)) {
        eprintln!("asm-check: {err}");
        return 1;
    }
    println!("asm-check: fast path division and frame checks passed");
    0
}

fn audit_pipeline_asm(dump: &str) -> Result<(), String> {
    for name in ["probe_log_u32", "probe_merge_pop"] {
        let marker = format!("<{name}>:");
        let (_, body) = dump.split_once(&marker).ok_or_else(|| format!("missing {name}"))?;
        let body = body.split("\n\n").next().unwrap_or("");
        if body.trim().is_empty() {
            return Err(format!("{name} has no instructions"));
        }
        if body
            .split_whitespace()
            .any(|word| matches!(word, "div" | "divq" | "divl" | "idiv" | "idivq" | "idivl"))
        {
            return Err(format!("{name} contains hardware division"));
        }
    }
    Ok(())
}

pub fn mca_gate_with(artifacts: &Artifacts) -> i32 {
    let assembly = match std::fs::read_to_string(&artifacts.assembly) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("mca-gate: {err}");
            return 1;
        }
    };
    for symbol in ["probe_format_u64", "probe_header_format", "probe_heap_node_pass"] {
        let region = match mca_region(&assembly, symbol) {
            Ok(region) => region,
            Err(err) => {
                eprintln!("mca-gate: {err}");
                return 1;
            }
        };
        let path = std::env::temp_dir().join(format!("{symbol}.mca.s"));
        if let Err(err) = std::fs::write(&path, region) {
            eprintln!("mca-gate: {err}");
            return 1;
        }
        let mca_path = path.display().to_string();
        let report =
            match tool_output("llvm-mca", &["-mcpu=native", "--bottleneck-analysis", &mca_path]) {
                Ok(text) => text,
                Err(err) => {
                    eprintln!("{err}");
                    return 1;
                }
            };
        let throughput = match block_rthroughput(&report) {
            Ok(value) => value,
            Err(err) => {
                eprintln!("mca-gate: {symbol}: {err}");
                return 1;
            }
        };
        let budget = mca_budget(symbol);
        if throughput > budget {
            eprintln!("mca-gate: {symbol} Block RThroughput {throughput} exceeds budget {budget}");
            return 1;
        }
        println!("mca-gate: {symbol} Block RThroughput {throughput} budget {budget}");
    }
    0
}

pub fn size_report_with(artifacts: &Artifacts) -> i32 {
    for path in [&artifacts.object, &artifacts.rlib] {
        let shown = path.display().to_string();
        let text = match tool_output("llvm-size", &["-A", "-d", &shown]) {
            Ok(text) => text,
            Err(err) => {
                eprintln!("{err}");
                return 1;
            }
        };
        println!("{text}");
        if !total_positive(&text) {
            eprintln!("size-report: {} has no positive Total", path.display());
            return 1;
        }
    }
    println!("size-report: object and rlib reported");
    0
}

pub(crate) fn workspace_root() -> Result<PathBuf, String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::canonicalize(manifest.join("../..")).map_err(|err| format!("workspace root: {err}"))
}

pub(crate) fn cargo_cmd() -> Result<Command, String> {
    let program = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let mut cmd = Command::new(&program);
    for (key, _) in std::env::vars() {
        if key.starts_with("CARGO") || key == "RUSTFLAGS" || key == "CARGO_ENCODED_RUSTFLAGS" {
            cmd.env_remove(&key);
        }
    }
    cmd.env("CARGO", program);
    Ok(cmd)
}

fn target_dir(root: &Path) -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => root.join("target"),
    }
}

fn assert_llvm_major() -> Result<(), String> {
    let mut rustc_cmd = Command::new("rustc");
    rustc_cmd.arg("-Vv");
    let rustc = run(&mut rustc_cmd, "rustc -Vv")?;
    let rustc_text = String::from_utf8_lossy(&rustc.stdout);
    let rustc_major = llvm_major(&rustc_text).ok_or("rustc -Vv has no LLVM version")?;
    let mut nm_cmd = Command::new("llvm-nm");
    nm_cmd.arg("--version");
    let tool = run(&mut nm_cmd, "llvm-nm --version")?;
    let tool_text = String::from_utf8_lossy(&tool.stdout);
    let tool_major = llvm_major(&tool_text).ok_or("llvm-nm --version has no LLVM version")?;
    if rustc_major != tool_major {
        return Err(format!("LLVM major mismatch: rustc {rustc_major} vs llvm-nm {tool_major}"));
    }
    Ok(())
}

fn llvm_major(text: &str) -> Option<u32> {
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        let Some(at) = lower.find("llvm version") else {
            continue;
        };
        let rest = line[at + "llvm version".len()..].trim().trim_start_matches(':').trim();
        let number: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
        return number.parse().ok();
    }
    None
}

fn find_artifacts(target: &Path) -> Result<Artifacts, String> {
    let mut ir_files = Vec::new();
    collect(target, "zerolog_probes", ".ll", &mut ir_files);
    ir_files.sort_by_key(|path| mtime(path));
    ir_files.reverse();
    for ir in ir_files {
        let Ok(text) = std::fs::read_to_string(&ir) else {
            continue;
        };
        if !text.contains("@probe_format_u64") {
            continue;
        }
        let Some(hash) = artifact_hash(&ir) else {
            continue;
        };
        let dir = ir.parent().unwrap_or(Path::new("."));
        let object = dir.join(format!("zerolog_probes-{hash}.o"));
        let assembly = dir.join(format!("zerolog_probes-{hash}.s"));
        let rlib = dir.join(format!("libzerolog_probes-{hash}.rlib"));
        if object.is_file() && assembly.is_file() && rlib.is_file() {
            return Ok(Artifacts { ir, assembly, object, rlib });
        }
    }
    Err("probe LLVM artifacts were not emitted".to_owned())
}

fn artifact_hash(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let rest = name.strip_prefix("zerolog_probes-")?;
    let hash = rest.rsplit_once('.')?.0;
    (!hash.is_empty()).then(|| hash.to_owned())
}

fn collect(dir: &Path, stem: &str, suffix: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == "incremental" || name == ".fingerprint" {
                continue;
            }
            collect(&path, stem, suffix, out);
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(stem) && name.ends_with(suffix))
        {
            out.push(path);
        }
    }
}

fn mtime(path: &Path) -> SystemTime {
    std::fs::metadata(path).and_then(|meta| meta.modified()).unwrap_or(SystemTime::UNIX_EPOCH)
}

fn sizes_present(nm: &str) -> Result<(), String> {
    for probe in PROBES {
        let sizes: Vec<_> = nm
            .lines()
            .filter_map(|line| {
                let columns: Vec<_> = line.split_whitespace().collect();
                match columns.as_slice() {
                    [_, size, _, name] if *name == probe => Some(*size),
                    _ => None,
                }
            })
            .collect();
        let [size] = sizes.as_slice() else {
            return Err(format!("expected one llvm-nm symbol {probe}"));
        };
        let bytes = u64::from_str_radix(size.trim_start_matches("0x"), 16)
            .map_err(|_| format!("{probe} size is not hex"))?;
        if bytes == 0 {
            return Err(format!("{probe} has zero size"));
        }
    }
    Ok(())
}

pub(crate) fn audit_ir(ir: &str) -> Result<(), String> {
    let functions = parse_functions(ir)?;
    let heap = functions
        .iter()
        .filter(|function| function.name == "probe_heap_node_pass")
        .collect::<Vec<_>>();
    if heap.len() != 1 {
        return Err(format!("expected one probe_heap_node_pass, found {}", heap.len()));
    }
    if signature_attr(&heap[0].signature, "sret") || signature_attr(&heap[0].signature, "byval") {
        return Err("probe_heap_node_pass uses sret or byval".to_owned());
    }
    let weights = branch_weights(ir);
    for name in ["probe_format_u64", "probe_header_format", "probe_log_u32", "probe_merge_pop"] {
        let matches: Vec<_> = functions.iter().filter(|function| function.name == name).collect();
        if matches.len() != 1 {
            return Err(format!("expected one {name}, found {}", matches.len()));
        }
        let hot = hot_blocks(&matches[0].body, &weights)?;
        for block in hot {
            for line in block.lines() {
                if let Some(op) = register_divisor(line) {
                    return Err(format!("{name} hot block contains {op} by a register"));
                }
            }
        }
    }
    Ok(())
}

struct Function {
    name: String,
    signature: String,
    body: String,
}

fn parse_functions(ir: &str) -> Result<Vec<Function>, String> {
    let lines: Vec<&str> = ir.lines().collect();
    let mut functions = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if !lines[index].trim_start().starts_with("define ") {
            index += 1;
            continue;
        }
        let mut signature = String::new();
        while index < lines.len() {
            signature.push(' ');
            signature.push_str(lines[index].trim());
            if lines[index].contains('{') {
                break;
            }
            index += 1;
        }
        if index >= lines.len() {
            return Err("unterminated LLVM define".to_owned());
        }
        let body_at = index;
        let mut depth = 0_i32;
        while index < lines.len() {
            depth += lines[index].matches('{').count() as i32;
            depth -= lines[index].matches('}').count() as i32;
            if depth == 0 {
                break;
            }
            index += 1;
        }
        if depth != 0 {
            return Err("unbalanced LLVM function body".to_owned());
        }
        let name = ir_name(&signature).ok_or("LLVM define has no name")?;
        functions.push(Function { name, signature, body: lines[body_at..=index].join("\n") });
        index += 1;
    }
    Ok(functions)
}
fn ir_name(signature: &str) -> Option<String> {
    let after = signature.split_once('@')?.1;
    if let Some(quoted) = after.strip_prefix('"') {
        Some(quoted.split_once('"')?.0.to_owned())
    } else {
        Some(after.split_once('(')?.0.trim().to_owned())
    }
}
fn signature_attr(signature: &str, attr: &str) -> bool {
    signature
        .split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | '(' | ')'))
        .any(|word| word == attr || word.starts_with(&format!("{attr}(")))
}

fn branch_weights(ir: &str) -> BTreeMap<String, (u64, u64)> {
    let mut weights = BTreeMap::new();
    for line in ir.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix('!') else { continue };
        let Some((id, payload)) = rest.split_once('=') else { continue };
        if !payload.contains("branch_weights") {
            continue;
        }
        // `i32` is a type token. The weights are the integers that follow it,
        // so a digit scan of the whole line would treat the 32 as a weight.
        let tokens: Vec<&str> = payload.split_whitespace().collect();
        let mut numbers = Vec::new();
        for pair in tokens.windows(2) {
            let kind = pair[0].trim_matches(|ch: char| ch == ',' || ch == '{' || ch == '}');
            if kind != "i32" && kind != "i64" {
                continue;
            }
            let number: String = pair[1].chars().filter(|ch| ch.is_ascii_digit()).collect();
            if let Ok(value) = number.parse::<u64>() {
                numbers.push(value);
            }
        }
        if numbers.len() >= 2 {
            weights.insert(format!("!{}", id.trim()), (numbers[0], numbers[1]));
        }
    }
    weights
}

/// `udiv`/`sdiv`/`urem`/`srem` are violations only when the divisor is an SSA register.
/// A literal divisor is a compile-time constant that instruction selection lowers to a multiply.
fn register_divisor(line: &str) -> Option<&'static str> {
    let tokens: Vec<&str> = line
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .filter(|token| !token.is_empty())
        .collect();
    let op_at =
        tokens.iter().position(|token| matches!(*token, "udiv" | "sdiv" | "urem" | "srem"))?;
    let opcode = match tokens[op_at] {
        "udiv" => "udiv",
        "sdiv" => "sdiv",
        "urem" => "urem",
        "srem" => "srem",
        _ => return None,
    };
    let mut cursor = op_at + 1;
    while cursor < tokens.len() && matches!(tokens[cursor], "exact" | "nsw" | "nuw") {
        cursor += 1;
    }
    if cursor < tokens.len() && tokens[cursor].starts_with('<') {
        while cursor < tokens.len() && !tokens[cursor].ends_with('>') {
            cursor += 1;
        }
        cursor += 1;
    } else if cursor < tokens.len()
        && tokens[cursor].starts_with('i')
        && tokens[cursor].len() > 1
        && tokens[cursor][1..].chars().all(|ch| ch.is_ascii_digit())
    {
        cursor += 1;
    }
    cursor += 1;
    let divisor = tokens.get(cursor)?;
    divisor.starts_with('%').then_some(opcode)
}

fn hot_blocks(body: &str, weights: &BTreeMap<String, (u64, u64)>) -> Result<Vec<String>, String> {
    let blocks = split_blocks(body);
    if blocks.is_empty() {
        return Err("function has no basic blocks".to_owned());
    }
    let mut index = BTreeMap::new();
    for (slot, block) in blocks.iter().enumerate() {
        index.insert(block.name.clone(), slot);
    }
    let mut hot = vec![false; blocks.len()];
    let mut stack = vec![0_usize];
    while let Some(slot) = stack.pop() {
        if hot[slot] {
            continue;
        }
        hot[slot] = true;
        for (target, cold) in edges(&blocks[slot], weights) {
            if cold {
                continue;
            }
            let next =
                index.get(&target).ok_or_else(|| format!("unresolved LLVM block: {target}"))?;
            stack.push(*next);
        }
    }
    let mut texts = Vec::new();
    for (slot, block) in blocks.iter().enumerate() {
        if hot[slot] {
            texts.push(block.text.clone());
        }
    }
    Ok(texts)
}

struct OwnedBlock {
    name: String,
    text: String,
}

fn split_blocks(body: &str) -> Vec<OwnedBlock> {
    let mut blocks = Vec::new();
    let mut name = Some("__entry".to_owned());
    let mut text = String::new();
    for (index, line) in body.lines().enumerate() {
        let line =
            if index == 0 { line.rsplit_once('{').map_or(line, |(_, rest)| rest) } else { line };
        if let Some(label) = label_name(line.trim()) {
            if let Some(name) = name.take()
                && !text.is_empty()
            {
                blocks.push(OwnedBlock { name, text: std::mem::take(&mut text) });
            }
            name = Some(label);
        } else if name.is_some()
            && !line.trim().is_empty()
            && !line.trim_start().starts_with("define ")
            && line.trim() != "}"
        {
            text.push_str(line);
            text.push('\n');
        }
    }
    if let Some(name) = name {
        blocks.push(OwnedBlock { name, text });
    }
    if blocks.is_empty() {
        blocks.push(OwnedBlock { name: "entry".to_owned(), text: body.to_owned() });
    }
    blocks
}

fn label_name(line: &str) -> Option<String> {
    let line = line.split(';').next()?.trim();
    if line.is_empty() || line.starts_with('!') || line.contains('=') || line.contains(' ') {
        return None;
    }
    let bare = line.strip_suffix(':')?.trim();
    (!bare.is_empty()).then(|| bare.trim_matches('"').to_owned())
}

fn edges(block: &OwnedBlock, weights: &BTreeMap<String, (u64, u64)>) -> Vec<(String, bool)> {
    let mut found = Vec::new();
    for line in block.text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("br i1 ") {
            let labels = cond_labels(trimmed);
            let pair = prof_id(trimmed).and_then(|id| weights.get(&id).copied());
            if labels.len() == 2 {
                let (true_cold, false_cold) = match pair {
                    Some((yes, no)) => (yes.saturating_mul(16) < no, no.saturating_mul(16) < yes),
                    None => (false, false),
                };
                found.push((labels[0].clone(), true_cold));
                found.push((labels[1].clone(), false_cold));
                continue;
            }
        }
        found.extend(
            cond_labels(trimmed.split(';').next().unwrap_or(""))
                .into_iter()
                .map(|label| (label, false)),
        );
    }
    found
}

fn trim_label(raw: &str) -> String {
    raw.trim().trim_end_matches(',').trim_matches('"').trim_start_matches('%').to_owned()
}

fn cond_labels(line: &str) -> Vec<String> {
    let mut labels = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("label %") {
        rest = &rest[at + "label %".len()..];
        let end = if let Some(quoted) = rest.strip_prefix('"') {
            quoted.find('"').map_or(rest.len(), |n| n + 2)
        } else {
            rest.find([',', ' ', '\t', ']']).unwrap_or(rest.len())
        };
        labels.push(trim_label(&rest[..end]));
        rest = &rest[end..];
    }
    labels
}

fn prof_id(line: &str) -> Option<String> {
    let at = line.find("!prof ")?;
    let token = line[at + "!prof ".len()..].split_whitespace().next()?;
    Some(token.trim_end_matches(',').to_owned())
}

fn mca_region(assembly: &str, symbol: &str) -> Result<String, String> {
    let mut active = false;
    let mut body = String::from(".text\n");
    let mut found = false;
    for line in assembly.lines() {
        let trimmed = line.trim();
        if trimmed == format!("{symbol}:") || trimmed == format!("\"{symbol}\":") {
            if found {
                return Err(format!("{symbol} assembly label is duplicated"));
            }
            active = true;
            found = true;
            body.push_str(symbol);
            body.push_str(":\n");
            continue;
        }
        if active && (trimmed.starts_with(".Lfunc_end") || trimmed.starts_with(".size")) {
            break;
        }
        if !active || trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('.') {
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
    if !found {
        return Err(format!("assembly missing {symbol}"));
    }
    Ok(body)
}

fn block_rthroughput(report: &str) -> Result<f64, String> {
    let mut values =
        report.lines().filter_map(|line| line.trim().strip_prefix("Block RThroughput:"));
    let raw = values.next().ok_or("missing Block RThroughput")?;
    let value: f64 = raw.trim().parse().map_err(|_| "invalid Block RThroughput")?;
    if values.next().is_some() || !value.is_finite() || value < 0.0 {
        return Err("expected one finite nonnegative Block RThroughput".to_owned());
    }
    Ok(value)
}

fn total_positive(report: &str) -> bool {
    report
        .lines()
        .filter_map(|line| line.trim().split_once(char::is_whitespace))
        .any(|(label, count)| label == "Total" && count.trim().parse::<u64>().is_ok_and(|n| n > 0))
}

fn tool_output(tool: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new(tool);
    cmd.args(args);
    let output = run(&mut cmd, tool)?;
    if !output.status.success() {
        return Err(format!("{tool} failed\n{}", tail(&output.stderr, &output.stdout)));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub(crate) fn run(cmd: &mut Command, label: &str) -> Result<Output, String> {
    cmd.output().map_err(|err| format!("spawn {label}: {err}"))
}

fn tail(stderr: &[u8], stdout: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(stdout));
    let start = text.ceil_char_boundary(text.len().saturating_sub(4000));
    text[start..].to_owned()
}

#[cfg(test)]
#[path = "tier1_tests.rs"]
mod tests;
