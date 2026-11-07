#![deny(clippy::unwrap_used, clippy::expect_used)]

mod tier1;
mod tier2;

fn main() {
    let mut args = std::env::args().skip(1);
    let command = args.next();
    let rest: Vec<String> = args.collect();
    let code = match command.as_deref() {
        Some("abi-check") => tier1::abi_check(),
        Some("ir-check") => tier1::ir_check(),
        Some("asm-check") => tier1::asm_check(),
        Some("mca-gate") => tier1::mca_gate(),
        Some("size-report") => tier1::size_report(),
        Some("gem5-gate") => tier2::gem5_gate(),
        Some("pmu-gate") => tier2::pmu_gate(),
        Some("verify") => verify(&rest),
        Some("help" | "-h" | "--help") => {
            usage();
            0
        }
        Some(other) => {
            eprintln!("unknown command {other}");
            usage();
            2
        }
        None => {
            usage();
            2
        }
    };
    std::process::exit(code);
}

fn verify(args: &[String]) -> i32 {
    let tier2 = args.iter().any(|arg| arg == "--tier2");
    let artifacts = match tier1::build_probes() {
        Ok(artifacts) => artifacts,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    let mut failed = false;
    for (name, code) in [
        ("abi-check", tier1::abi_check_with(&artifacts)),
        ("ir-check", tier1::ir_check_with(&artifacts)),
        ("asm-check", tier1::asm_check_with(&artifacts)),
        ("mca-gate", tier1::mca_gate_with(&artifacts)),
        ("size-report", tier1::size_report_with(&artifacts)),
    ] {
        if code != 0 {
            eprintln!("verify: {name} exited {code}");
            failed = true;
        }
    }
    if tier2 {
        for (name, code) in [("gem5-gate", tier2::gem5_gate()), ("pmu-gate", tier2::pmu_gate())] {
            if code != 0 {
                eprintln!("verify: {name} exited {code}");
                failed = true;
            }
        }
    }
    i32::from(failed)
}

fn usage() {
    eprintln!(
        "usage: cargo xtask <abi-check|ir-check|asm-check|mca-gate|size-report|gem5-gate|pmu-gate|verify [--tier2]>"
    );
}
