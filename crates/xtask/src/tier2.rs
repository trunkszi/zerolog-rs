#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use crate::tier1::{cargo_cmd, run, workspace_root};
const POINTS: [&str; 3] = ["0000000512", "0000002560", "0000004608"];
const SLOPE_LIMIT: f64 = 0.05;
const SLOT_WIDTH: f64 = 8.0;

#[derive(Clone, Copy)]
struct Sample {
    iterations: f64,
    cycles: f64,
    ops: f64,
    insts: f64,
    idle: f64,
    renamed: f64,
    rename_idle: f64,
    squash: f64,
    violations: f64,
}
#[derive(Debug)]
struct Tma {
    activity: f64,
    buckets: [f64; 4],
}

pub fn gem5_gate() -> i32 {
    match gem5_run() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("gem5-gate: {error}");
            1
        }
    }
}
fn gem5_run() -> Result<(), String> {
    let gem5 = gem5_binary().ok_or("gem5.opt not found")?;
    let script = PathBuf::from("/opt/gem5/configs/deprecated/example/se.py");
    if !script.is_file() {
        return Err("gem5 se.py is missing".into());
    }
    let bin = build_harness()?;
    let mut samples = Vec::new();
    for point in POINTS {
        let sample = simulate(&gem5, &script, &bin, point)?;
        println!(
            "gem5-gate: N={point} cycles={} uops={} insts={}",
            sample.cycles, sample.ops, sample.insts
        );
        samples.push(sample);
    }
    slope_report(&samples)
}
pub fn pmu_gate() -> i32 {
    match build_harness().and_then(|bin| collect_pmu(&bin)) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("pmu-gate: {error}");
            1
        }
    }
}
fn gem5_binary() -> Option<PathBuf> {
    let x86 = PathBuf::from("/opt/gem5/build/X86/gem5.opt");
    if x86.is_file() { Some(x86) } else { which("gem5.opt") }
}
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|path| path.is_file())
}
fn build_harness() -> Result<PathBuf, String> {
    let root = workspace_root()?;
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let mut command = cargo_cmd()?;
    command
        .current_dir(&root)
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_TERM_COLOR", "never")
        .args(["build", "--package", "zerolog-probes", "--bin", "gem5_harness", "--release"]);
    let output = run(&mut command, "cargo build gem5_harness")?;
    require_success(&output, "harness build")?;
    let binary = target.join("release/gem5_harness");
    if binary.is_file() { Ok(binary) } else { Err(format!("missing harness {}", binary.display())) }
}
fn require_success(output: &Output, label: &str) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("{label}: {}\n{}", output.status, tail(&output.stderr, &output.stdout)))
    }
}
fn checksum(text: &str) -> Result<u64, String> {
    let values: Vec<_> =
        text.lines().filter_map(|line| line.trim().strip_prefix("checksum ")).collect();
    let [value] = values.as_slice() else {
        return Err("expected exactly one checksum line".into());
    };
    value.parse().map_err(|_| "invalid checksum".into())
}
fn verify_parity(host: (u64, i32), guest: (u64, i32)) -> Result<(), String> {
    if host != guest || host.1 != (host.0 & 0x7f) as i32 {
        return Err("host/guest checksum or exit status mismatch".into());
    }
    Ok(())
}
fn guest_exit(text: &str) -> Result<i32, String> {
    let exits: Vec<_> = text.lines().filter(|line| line.starts_with("Exiting @ tick ")).collect();
    if exits.len() != 1 || !exits[0].ends_with("because exiting with last active thread context") {
        return Err("gem5 did not report normal guest termination".into());
    }
    let codes: Vec<_> = text
        .lines()
        .filter_map(|line| line.strip_prefix("Simulated exit code not 0! Exit code is "))
        .collect();
    match codes.as_slice() {
        [] => Ok(0),
        [code] => code
            .parse::<i32>()
            .ok()
            .filter(|code| *code > 0 && *code <= 127)
            .ok_or_else(|| "invalid guest exit status".into()),
        _ => Err("ambiguous guest exit status".into()),
    }
}
fn simulate(gem5: &Path, script: &Path, bin: &Path, point: &str) -> Result<Sample, String> {
    let host = run(Command::new(bin).arg(point), "host harness")?;
    let host_sum = checksum(&String::from_utf8_lossy(&host.stdout))?;
    let host_code = host.status.code().ok_or("host harness terminated by signal")?;
    verify_parity((host_sum, host_code), (host_sum, (host_sum & 0x7f) as i32))?;
    let out = std::env::temp_dir().join(format!("zl-gem5-{}-{point}", std::process::id()));
    std::fs::create_dir(&out).map_err(|error| format!("create {}: {error}", out.display()))?;
    let mut command = Command::new(gem5);
    command
        .env("GLIBC_TUNABLES", "glibc.pthread.rseq=0")
        .arg(format!("--outdir={}", out.display()))
        .args(["--stats-file=stats.txt"])
        .arg(script)
        .args(["--cpu-type=X86O3CPU", "--caches", "--l2cache"])
        .arg(format!("--cmd={}", bin.display()))
        .arg(format!("--options={point} --gem5"));
    let output = run(&mut command, "gem5.opt")?;
    let console = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::write(out.join("console.txt"), &console).map_err(|e| e.to_string())?;
    require_success(&output, "gem5 process")?;
    verify_parity((host_sum, host_code), (checksum(&console)?, guest_exit(&console)?))?;
    let config = std::fs::read_to_string(out.join("config.ini")).map_err(|e| e.to_string())?;
    let widths: Vec<_> =
        config.lines().filter_map(|line| line.strip_prefix("renameWidth=")).collect();
    if widths.as_slice() != ["8"] {
        return Err("expected a single width-8 O3 rename pipeline".into());
    }
    let stats = std::fs::read_to_string(out.join("stats.txt")).map_err(|e| e.to_string())?;
    println!(
        "gem5-gate: checksum={host_sum} exit={host_code} parity passed; artifacts {}",
        out.display()
    );
    let blocks: Vec<_> =
        stats.split("---------- Begin Simulation Statistics ----------").skip(1).collect();
    if blocks.len() != 2 {
        return Err("expected one explicit ROI dump and one final stats block".into());
    }
    let (roi, _) = blocks[0]
        .split_once("---------- End Simulation Statistics   ----------")
        .ok_or("incomplete ROI stats block")?;
    parse_sample(roi, point.parse().map_err(|_| "invalid phase N")?)
        .ok_or_else(|| "missing or invalid O3 counters".into())
}
fn parse_sample(stats: &str, iterations: f64) -> Option<Sample> {
    Some(Sample {
        iterations,
        cycles: stat(stats, "numCycles")?,
        ops: stat(stats, "commitStats0.numOps")?,
        insts: stat(stats, "commitStats0.numInsts")?,
        idle: stat(stats, "idleCycles")?,
        renamed: stat(stats, "rename.renamedInsts")?,
        rename_idle: stat(stats, "rename.status::Idle")?,
        squash: stat(stats, "rename.status::Squashing")?,
        violations: stat(stats, "iew.memOrderViolationEvents")?,
    })
}
fn stat(stats: &str, key: &str) -> Option<f64> {
    let key = format!("system.cpu.{key}");
    let mut values = stats.lines().filter_map(|line| {
        let mut parts = line.split_whitespace();
        (parts.next()? == key).then(|| parts.next()).flatten()
    });
    let value = values.next()?.parse::<f64>().ok()?;
    (values.next().is_none() && value.is_finite() && value >= 0.0).then_some(value)
}
fn quadrants(left: Sample, right: Sample) -> Result<[f64; 4], String> {
    if left.violations != 0.0 || right.violations != 0.0 {
        return Err("gem5 ROI reported non-zero memory/pipeline violations".into());
    }
    let delta = [
        right.cycles - left.cycles,
        right.ops - left.ops,
        right.insts - left.insts,
        right.idle - left.idle,
        right.renamed - left.renamed,
        right.rename_idle - left.rename_idle,
        right.squash - left.squash,
        right.violations - left.violations,
    ];
    if delta.iter().any(|v| !v.is_finite() || *v < 0.0)
        || delta[0] <= 0.0
        || delta[1] <= 0.0
        || delta[2] <= 0.0
        || delta[7] != 0.0
    {
        return Err("invalid ROI counter delta or nonzero memOrderViolationEvents delta".into());
    }
    let slots = (delta[0] - delta[3]) * SLOT_WIDTH;
    if !slots.is_finite() || slots <= 0.0 {
        return Err("invalid O3 active-slot denominator".into());
    }
    let retiring = 100.0 * delta[1] / slots;
    let bad = 100.0 * ((delta[4] - delta[1]).max(0.0) + SLOT_WIDTH * delta[6]) / slots;
    let frontend = 100.0 * SLOT_WIDTH * delta[5] / slots;
    let raw_sum = retiring + bad + frontend;
    let buckets = [retiring, bad, frontend, (100.0 - raw_sum).clamp(0.0, 100.0)];
    if buckets.iter().any(|v| !v.is_finite() || *v < 0.0) || raw_sum > 105.0 {
        return Err("invalid O3 rename-slot quadrant attribution".into());
    }
    Ok(buckets)
}
fn slope_report(samples: &[Sample]) -> Result<(), String> {
    if samples.len() != 3
        || samples.iter().zip(POINTS).any(|(s, n)| s.iterations != n.parse::<f64>().unwrap_or(0.0))
    {
        return Err("expected the three fixed phase points".into());
    }
    for pair in samples.windows(2) {
        let buckets = quadrants(pair[0], pair[1])?;
        let cycles = (pair[1].cycles - pair[0].cycles) / (pair[1].iterations - pair[0].iterations);
        println!(
            "gem5-gate: {}->{} cycles/op={cycles:.4}; o3-rename-slot={buckets:?}; memory-order delta=0",
            pair[0].iterations, pair[1].iterations
        );
    }
    let deviation = slope_deviation(samples[0].cycles, samples[1].cycles, samples[2].cycles)?;
    println!("gem5-gate: slope deviation {deviation:.4}");
    if deviation > SLOPE_LIMIT {
        return Err(format!("nonlinear O3 ROI: {deviation:.4} > {SLOPE_LIMIT}"));
    }
    Ok(())
}
fn slope_deviation(low: f64, mid: f64, high: f64) -> Result<f64, String> {
    let first = mid - low;
    let second = high - mid;
    if !first.is_finite() || !second.is_finite() || first <= 0.0 || second <= 0.0 {
        return Err("invalid cycle slope".into());
    }
    Ok((second - first).abs() / first)
}
fn collect_pmu(bin: &Path) -> Result<(), String> {
    let out = std::env::temp_dir().join(format!("zl-pmu-{}", std::process::id()));
    std::fs::create_dir(&out).map_err(|e| e.to_string())?;
    let status = std::fs::read_to_string("/proc/self/status").map_err(|e| e.to_string())?;
    let allowed = status
        .lines()
        .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))
        .ok_or("missing CPU affinity")?;
    let cpu: u32 = allowed
        .trim()
        .split([',', '-'])
        .next()
        .ok_or("empty affinity")?
        .parse()
        .map_err(|_| "invalid CPU affinity")?;
    let csv = out.join("cumulative.csv");
    let mut command = Command::new("AMDuProfPcm");
    command
        .args(["-C", "-m", "ipc,pipeline_util", "-c"])
        .arg(format!("core={cpu}"))
        .arg("-o")
        .arg(&csv)
        .args(["--", "taskset", "-c"])
        .arg(cpu.to_string())
        .args(["sh", "-c", "\"$1\" --pmu; zl_pmu_status=$?; printf 'workload_exit %s\\n' \"$zl_pmu_status\"; exit \"$zl_pmu_status\"", "zerolog-pmu"])
        .arg(bin);
    let output = run(&mut command, "AMDuProfPcm")?;
    std::fs::write(
        out.join("console.txt"),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
    .map_err(|e| e.to_string())?;
    verify_pmu_output(&output)?;
    let report = std::fs::read_to_string(&csv).map_err(|e| e.to_string())?;
    let metrics = parse_pmu(&report)?;
    println!(
        "pmu-gate: AMDuProfPcm CPU={cpu} positive activity={} TMA={:?}; artifacts {}",
        metrics.activity,
        metrics.buckets,
        out.display()
    );
    Ok(())
}
fn verify_pmu_output(output: &Output) -> Result<(), String> {
    require_success(output, "AMDuProfPcm")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    checksum(&stdout)?;
    let exits: Vec<_> =
        stdout.lines().filter_map(|line| line.strip_prefix("workload_exit ")).collect();
    if exits.as_slice() != ["0"] {
        return Err("PMU workload did not exit successfully".into());
    }
    Ok(())
}
fn metric_key(text: &str) -> String {
    text.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}
fn parse_pmu(report: &str) -> Result<Tma, String> {
    let mut metrics = BTreeMap::new();
    let rows: Vec<Vec<_>> = report
        .lines()
        .map(|line| line.split(',').map(|v| v.trim().trim_matches('"')).collect())
        .collect();
    for (i, row) in rows.iter().enumerate() {
        if row.len() == 2
            && let Ok(value) = row[1].trim_end_matches('%').parse::<f64>()
        {
            metrics.insert(metric_key(row[0]), value);
        }
        if let Some(values) = rows.get(i + 1)
            && row.len() >= 4
            && row.len() == values.len()
        {
            for (label, value) in row.iter().zip(values) {
                if let Ok(value) = value.trim_end_matches('%').parse::<f64>() {
                    metrics.insert(metric_key(label), value);
                }
            }
        }
    }
    let activity = ["cyclesnotinhalt", "cycles"]
        .iter()
        .filter_map(|key| metrics.get(*key).copied())
        .find(|v| v.is_finite() && *v > 0.0)
        .ok_or("PMU output did not contain a positive cycle counter")?;
    let mut buckets = [0.0; 4];
    for (i, key) in
        ["retiring", "frontendbound", "badspeculation", "backendbound"].iter().enumerate()
    {
        buckets[i] = *metrics.get(*key).ok_or_else(|| format!("missing numeric TMA {key}"))?;
    }
    if buckets.iter().any(|v| !v.is_finite() || *v < 0.0)
        || buckets[0] <= 0.0
        || !(90.0..=110.0).contains(&buckets.iter().sum())
    {
        return Err("invalid TMA percentages or quadrant sum".into());
    }
    Ok(Tma { activity, buckets })
}

pub(crate) fn audit_asm(dump: &str) -> Result<(), String> {
    for symbol in ["probe_level_gate_check", "probe_spsc_alloc_push"] {
        let insns = symbol_insns(dump, symbol)?;
        let fast = fast_path(&insns)?;
        if fast.is_empty() {
            return Err(format!("{symbol} fast path is empty"));
        }
        for insn in fast {
            if integer_div(&insn.text) || mnemonic(&insn.text) == "ud2" {
                return Err(format!("{symbol} fast path contains {}", insn.text));
            }
        }
    }
    for symbol in ["probe_format_u64", "probe_header_format"] {
        let insns = symbol_insns(dump, symbol)?;
        for insn in &insns {
            if integer_div(&insn.text) || mnemonic(&insn.text) == "ud2" {
                return Err(format!("{symbol} contains {}", insn.text));
            }
        }
    }
    let heap = symbol_insns(dump, "probe_heap_node_pass")?;
    for insn in &heap {
        if frame_sub(&insn.text) {
            return Err(format!("probe_heap_node_pass allocates a frame: {}", insn.text));
        }
    }
    Ok(())
}

struct Insn {
    addr: u64,
    text: String,
}

fn symbol_insns(dump: &str, symbol: &str) -> Result<Vec<Insn>, String> {
    let mut active = false;
    let mut seen = false;
    let mut insns = Vec::new();
    for line in dump.lines() {
        if let Some(name) = symbol_header(line) {
            active = name == symbol;
            seen |= active;
            continue;
        }
        if !active {
            continue;
        }
        if let Some(insn) = parse_insn(line) {
            insns.push(insn);
        }
    }
    if !seen || insns.is_empty() {
        return Err(format!("missing disassembly for {symbol}"));
    }
    Ok(insns)
}

fn symbol_header(line: &str) -> Option<&str> {
    let line = line.trim();
    let (addr, rest) = line.split_once(" <")?;
    if !addr.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    rest.strip_suffix(">:")
}

fn parse_insn(line: &str) -> Option<Insn> {
    let line = line.trim();
    let (addr, rest) = line.split_once(':')?;
    if !addr.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    let mut mnemonic = None;
    let mut kept = Vec::new();
    for token in rest.split_whitespace() {
        if mnemonic.is_none() && token.chars().all(|ch| ch.is_ascii_hexdigit()) && token.len() <= 2
        {
            continue;
        }
        if mnemonic.is_none() {
            mnemonic = Some(token.to_owned());
        }
        kept.push(token);
    }
    let text = kept.join(" ");
    if text.is_empty() || (text.starts_with('R') && text.contains("_X86_")) {
        return None;
    }
    Some(Insn { addr: u64::from_str_radix(addr, 16).ok()?, text })
}

fn fast_path(insns: &[Insn]) -> Result<Vec<&Insn>, String> {
    let by_addr: BTreeMap<_, _> =
        insns.iter().enumerate().map(|(i, insn)| (insn.addr, i)).collect();
    let mut hot = vec![false; insns.len()];
    let mut stack = vec![0_usize];
    while let Some(slot) = stack.pop() {
        if hot[slot] {
            continue;
        }
        hot[slot] = true;
        let text = &insns[slot].text;
        let op = mnemonic(text);
        if op.starts_with('j') || op.starts_with("loop") {
            if !cold_target(text) {
                let target = jump_addr(text)
                    .and_then(|addr| by_addr.get(&addr).copied())
                    .ok_or_else(|| format!("unresolved assembly branch: {text}"))?;
                stack.push(target);
            }
            if op == "jmp" {
                continue;
            }
        }
        if !op.starts_with("ret") && op != "ud2" && slot + 1 < insns.len() {
            stack.push(slot + 1);
        }
    }
    Ok(insns.iter().enumerate().filter(|(slot, _)| hot[*slot]).map(|(_, insn)| insn).collect())
}
fn cold_target(text: &str) -> bool {
    let Some((_, target)) = text.split_once('<') else { return false };
    [
        "cold",
        "slow",
        "store_second",
        "prepare_negative",
        "ring_doorbell",
        "panic",
        "alloc_slow",
        "flush_slow",
    ]
    .iter()
    .any(|name| target.contains(name))
}

fn mnemonic(text: &str) -> &str {
    text.split_whitespace().next().unwrap_or("")
}

fn jump_addr(text: &str) -> Option<u64> {
    for token in text.split_whitespace().skip(1) {
        let token = token.trim_start_matches("0x");
        if token.chars().all(|ch| ch.is_ascii_hexdigit())
            && let Ok(addr) = u64::from_str_radix(token, 16)
        {
            return Some(addr);
        }
    }
    None
}

fn integer_div(text: &str) -> bool {
    matches!(
        mnemonic(text),
        "div" | "divb" | "divw" | "divl" | "divq" | "idiv" | "idivb" | "idivw" | "idivl" | "idivq"
    )
}

fn frame_sub(text: &str) -> bool {
    let mut parts = text.split_whitespace();
    if parts.next() != Some("sub") {
        return false;
    }
    matches!(parts.next().map(|op| op.trim_end_matches(',')), Some("rsp" | "esp"))
}

fn tail(stderr: &[u8], stdout: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(stdout));
    let start = text.ceil_char_boundary(text.len().saturating_sub(4000));
    text[start..].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(n: f64) -> Sample {
        Sample {
            iterations: n,
            cycles: 1000.0 + 10.0 * n,
            ops: 100.0 + 20.0 * n,
            insts: 50.0 + 10.0 * n,
            idle: 10.0,
            renamed: 200.0 + 24.0 * n,
            rename_idle: 20.0 + n,
            squash: 2.0,
            violations: 0.0,
        }
    }
    const VERTICAL: &str = "Cycles Not in Halt,123456\nRetired Instructions,500000\nRetiring,57\nFrontend_Bound,8\nBad_Speculation,5\nBackend_Bound,30\n";
    #[test]
    fn pmu_rejects_header_only_reports() {
        assert!(parse_pmu("Cycles Not in Halt,Retiring,Frontend_Bound,Bad_Speculation,Backend_Bound\nchecksum 9\n").is_err());
        assert!(
            parse_pmu("Retired Instructions,Retired Macro Ops,Cycles Not in Halt,IPC\n").is_err()
        );
        assert!(parse_pmu("1234 cycles\n5678 instructions\n").is_err());
    }
    #[test]
    fn pmu_accepts_numeric_tma_rows() {
        assert!(parse_pmu(VERTICAL).is_ok());
        assert!(
            parse_pmu(
                "Cycles,Retiring,Frontend Bound,Bad Speculation,Backend Bound\n1234,57,8,5,30\n"
            )
            .is_ok()
        );
        for invalid in [
            VERTICAL.replace("57\n", "NaN\n"),
            VERTICAL.replace("57\n", "0\n"),
            VERTICAL.replace("57\n", "200\n"),
            VERTICAL.replace("123456", "0").replace("500000", "0"),
        ] {
            assert!(parse_pmu(&invalid).is_err());
        }
    }
    #[test]
    fn checksum_and_exit_parity_required() {
        assert!(verify_parity((9, 9), (9, 9)).is_ok());
        assert!(verify_parity((9, 9), (10, 9)).is_err());
        assert!(verify_parity((9, 9), (9, 8)).is_err());
        assert!(verify_parity((9, 8), (9, 8)).is_err());
        assert!(checksum("checksum 9\nchecksum 9\n").is_err());
        let console = "checksum 9\nExiting @ tick 100 because exiting with last active thread context\nSimulated exit code not 0! Exit code is 9\n";
        assert_eq!(guest_exit(console), Ok(9));
        assert!(
            guest_exit(&console.replace(
                "because exiting with last active thread context",
                "because simulate() limit reached"
            ))
            .is_err()
        );
    }
    #[test]
    fn roi_memory_violations_are_rejected() {
        let valid = [sample(512.0), sample(2560.0), sample(4608.0)];
        assert!(slope_report(&valid).is_ok());
        let mut bad = valid;
        bad[1].violations += 1.0;
        assert!(slope_report(&bad).is_err());
    }
    #[test]
    fn invalid_quadrant_sums_are_rejected() {
        let left = sample(512.0);
        let mut right = sample(2560.0);
        right.ops = right.cycles * 16.0;
        assert!(quadrants(left, right).is_err());
        right = sample(2560.0);
        right.cycles = f64::MAX / 2.0;
        assert!(quadrants(left, right).is_err());
        right = sample(2560.0);
        right.rename_idle = f64::NAN;
        assert!(quadrants(left, right).is_err());
    }
    #[test]
    fn adjacent_slopes_reject_phase_breaks() {
        assert!(slope_deviation(100.0, 300.0, 500.0).is_ok_and(|d| d <= SLOPE_LIMIT));
        assert!(slope_deviation(100.0, 200.0, 800.0).is_ok_and(|d| d > SLOPE_LIMIT));
        assert!(slope_deviation(100.0, f64::NAN, 800.0).is_err());
        let mut bad = [sample(512.0), sample(2560.0), sample(4608.0)];
        bad[2].cycles += 100_000.0;
        assert!(slope_report(&bad).is_err());
    }
    #[test]
    fn stats_require_complete_numeric_fields() {
        let stats = "system.cpu.numCycles 330000\nsystem.cpu.commitStats0.numInsts 118922\nsystem.cpu.commitStats0.numOps 191609\nsystem.cpu.idleCycles 10\nsystem.cpu.rename.renamedInsts 200000\nsystem.cpu.rename.status::Idle 200\nsystem.cpu.rename.status::Squashing 3\nsystem.cpu.iew.memOrderViolationEvents 9\n";
        assert!(parse_sample(stats, 512.0).is_some());
        assert!(
            parse_sample(&stats.replace("system.cpu.rename.status::Idle 200\n", ""), 512.0)
                .is_none()
        );
        assert!(parse_sample(&stats.replace("330000", "NaN"), 512.0).is_none());
    }
    #[test]
    #[cfg(unix)]
    fn failed_tools_cannot_supply_evidence() {
        use std::os::unix::process::ExitStatusExt;
        let output = Output {
            status: std::process::ExitStatus::from_raw(7 << 8),
            stdout: VERTICAL.as_bytes().to_vec(),
            stderr: Vec::new(),
        };
        assert!(require_success(&output, "fixture").is_err());
        let mut output = output;
        output.status = std::process::ExitStatus::from_raw(0);
        output.stdout = b"checksum 9\nworkload_exit 7\n".to_vec();
        assert!(verify_pmu_output(&output).is_err());
        output.stdout = b"checksum 9\n".to_vec();
        assert!(verify_pmu_output(&output).is_err());
        output.stdout = b"checksum 9\nworkload_exit 0\n".to_vec();
        assert!(verify_pmu_output(&output).is_ok());
    }
    #[test]
    fn pmu_requires_positive_cycles() {
        for cycles in ["", "Cycles,0\n"] {
            let report = format!(
                "{cycles}IPC,1\nRetiring,57\nFrontend_Bound,8\nBad_Speculation,5\nBackend_Bound,30\n"
            );
            assert!(parse_pmu(&report).is_err());
        }
    }
    #[test]
    fn constant_roi_violations_fail() {
        let mut samples = [sample(512.0), sample(2560.0), sample(4608.0)];
        for sample in &mut samples {
            sample.violations = 1.0;
        }
        assert!(slope_report(&samples).is_err());
    }
    #[test]
    fn tail_handles_utf8_boundary() {
        let text = format!("{}a", "é".repeat(2001));
        assert_eq!(tail(text.as_bytes(), &[]), format!("{}a", "é".repeat(1999)));
    }
}
