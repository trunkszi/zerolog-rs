use super::*;

#[test]
fn heap_ir_rejects_sret() {
    let ir = "define void @probe_heap_node_pass(ptr sret(%HeapNode) %0) {\nentry:\n ret void\n}\n";
    assert!(audit_ir(ir).is_err());
}

fn fixture(format_body: &str) -> String {
    let mut ir = String::from(
        "define { i64, i64 } @probe_heap_node_pass(i64 %a, i64 %b) {\nentry:\n ret { i64, i64 } zeroinitializer\n}\n",
    );
    for name in ["probe_header_format", "probe_log_u32", "probe_merge_pop"] {
        ir.push_str(&format!("define i64 @{name}(i64 %v) {{\nentry:\n ret i64 %v\n}}\n"));
    }
    ir.push_str(&format!("define i64 @probe_format_u64(i64 %v, i64 %d) {{\n{format_body}\n}}\n"));
    ir
}
#[test]
fn divisor_rules_preserve_cold_exemptions() {
    assert!(audit_ir(&fixture("entry:\n %q = udiv i64 %v, 10\n ret i64 %q")).is_ok());
    assert!(audit_ir(&fixture("entry:\n %q = udiv i64 %v, %d\n ret i64 %q")).is_err());
    let cold = fixture(
        "entry:\n br i1 %v, label %hot, label %cold, !prof !1\nhot:\n ret i64 %v\ncold:\n %q = sdiv exact i64 %v, %d\n ret i64 %q",
    ) + "!1 = !{!\"branch_weights\", i32 2000, i32 1}\n";
    assert!(audit_ir(&cold).is_ok());
}
#[test]
fn pipeline_probes_reject_division() {
    for name in ["probe_log_u32", "probe_merge_pop"] {
        let ir = fixture("entry:\n ret i64 %v").replace(
            &format!("define i64 @{name}(i64 %v) {{\nentry:\n ret i64 %v"),
            &format!("define i64 @{name}(i64 %v) {{\nentry:\n %q = udiv i64 %v, %v\n ret i64 %q"),
        );
        assert!(audit_ir(&ir).is_err());
        let dump = "0 <probe_log_u32>:\n 0: c3 ret\n\n0 <probe_merge_pop>:\n 0: c3 ret\n";
        assert!(audit_pipeline_asm(dump).is_ok());
        let bad = dump
            .replace(&format!("<{name}>:\n 0: c3 ret"), &format!("<{name}>:\n 0: f7 f1 div ecx"));
        assert!(audit_pipeline_asm(&bad).is_err());
    }
}

fn asm_fixture(changed: &str, instructions: &str) -> String {
    let mut dump = String::new();
    for symbol in PROBES {
        let body = if symbol == changed { instructions } else { "0: c3 ret" };
        dump.push_str(&format!("0 <{symbol}>:\n {body}\n\n"));
    }
    dump
}
#[test]
fn asm_rejects_taken_div() {
    let dump = asm_fixture(
        "probe_level_gate_check",
        "0: 83 f9 05 cmp ecx, 5\n 3: 76 02 jbe 0x7\n 5: c3 ret\n 7: f7 f1 div ecx\n 9: c3 ret",
    );
    assert!(crate::tier2::audit_asm(&dump).is_err());
}
#[test]
fn asm_rejects_frame_and_format_div() {
    for symbol in ["probe_format_u64", "probe_header_format"] {
        let dump = asm_fixture(symbol, "0: f7 f1 div ecx\n 2: c3 ret");
        assert!(crate::tier2::audit_asm(&dump).is_err());
    }
    let dump = asm_fixture("probe_heap_node_pass", "0: 48 83 ec 10 sub rsp, 16\n 4: c3 ret");
    assert!(crate::tier2::audit_asm(&dump).is_err());
}

#[test]
fn ir_checks_implicit_entry_and_switch() {
    let implicit = fixture(" %q = udiv i64 %v, %d\n br label %done\ndone:\n ret i64 %q");
    assert!(audit_ir(&implicit).is_err());
    let switch = fixture(
        "entry:\n switch i64 %v, label %bad [\n i64 0, label %ok\n ]\nok:\n ret i64 %v\nbad:\n %q = udiv i64 %v, %d\n ret i64 %q",
    );
    assert!(audit_ir(&switch).is_err());
}
#[test]
fn asm_checks_reachable_traps() {
    let cold = asm_fixture(
        "probe_level_gate_check",
        "0: eb 02 jmp 0x4 <cold>\n 2: 0f 0b ud2\n 4: 0f 0b ud2",
    );
    assert!(crate::tier2::audit_asm(&cold).is_ok());
    let trap = asm_fixture(
        "probe_level_gate_check",
        "0: 74 02 je 0x4 <.Ltrap>\n 2: c3 ret\n 4: 0f 0b ud2",
    );
    assert!(crate::tier2::audit_asm(&trap).is_err());
}
#[test]
fn tail_handles_utf8_boundary() {
    let text = format!("{}a", "é".repeat(2001));
    assert_eq!(tail(text.as_bytes(), &[]), format!("{}a", "é".repeat(1999)));
}
#[test]
fn ir_checks_multiline_signature() {
    let ir = fixture("entry:\n %q = udiv i64 %v, %d\n ret i64 %q")
        .replace("@probe_format_u64(i64 %v, i64 %d) {", "@probe_format_u64(\n i64 %v, i64 %d) {");
    assert!(audit_ir(&ir).is_err());
}
