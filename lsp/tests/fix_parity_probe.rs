//! The `karac fix` vs quick-fix parity probe that scripts/fix-parity.py
//! drives (ignored: it reads `KARA_PARITY_DIR` and `KARAC`).
use kara_lsp::analysis::code_actions;
use lsp_types::{CodeActionKind, CodeActionOrCommand, Position, Range, TextEdit, Uri};
use std::str::FromStr;

fn offset(text: &str, p: Position) -> usize {
    let idx = kara_lsp::analysis::LineIndex::new(text);
    idx.offset(p)
}
fn apply(text: &str, edits: &[TextEdit]) -> String {
    let mut e: Vec<(usize, usize, String)> = edits
        .iter()
        .map(|x| {
            (
                offset(text, x.range.start),
                offset(text, x.range.end),
                x.new_text.clone(),
            )
        })
        .collect();
    e.sort_by_key(|x| std::cmp::Reverse(x.0));
    let mut out = text.to_string();
    for (a, b, t) in e {
        out.replace_range(a..b, &t);
    }
    out
}

#[test]
#[ignore]
#[allow(clippy::mutable_key_type)]
fn parity() {
    let dir = std::env::var("KARA_PARITY_DIR").unwrap();
    let karac = std::env::var("KARAC").unwrap();
    let work = std::env::temp_dir().join("kara-parity-work");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    let (mut same, mut differ, mut nofix) = (0, 0, 0);
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).unwrap();
        let p = work.join("main.kara");
        std::fs::write(&p, &text).unwrap();
        let _ = std::process::Command::new(&karac)
            .arg("fix")
            .arg(&p)
            .current_dir(&work)
            .output();
        let cli = std::fs::read_to_string(&p).unwrap();
        let uri = Uri::from_str(&format!("file://{}", p.display())).unwrap();
        let mut ed = text.clone();
        let mut steps = 0;
        while steps < 40 {
            let acts = code_actions(
                &ed,
                &uri,
                Range::new(Position::new(0, 0), Position::new(99999, 0)),
            );
            let batch = std::env::var("BATCH").is_ok();
            let mut all: Vec<TextEdit> = Vec::new();
            for a in acts {
                if let CodeActionOrCommand::CodeAction(a) = a {
                    if a.kind == Some(CodeActionKind::QUICKFIX) && a.is_preferred == Some(true) {
                        let edits = a
                            .edit
                            .unwrap()
                            .changes
                            .unwrap()
                            .into_values()
                            .next()
                            .unwrap();
                        // Skip an action overlapping one already taken.
                        let clash = edits.iter().any(|e| {
                            all.iter().any(|t| {
                                let (a0, a1) =
                                    (offset(&ed, e.range.start), offset(&ed, e.range.end));
                                let (b0, b1) =
                                    (offset(&ed, t.range.start), offset(&ed, t.range.end));
                                (a0 < b1 && b0 < a1)
                                    || (a0 == b0
                                        && (a0 == a1 || b0 == b1)
                                        && e.new_text == t.new_text)
                            })
                        });
                        if !clash {
                            all.extend(edits);
                        }
                        if !batch && !all.is_empty() {
                            break;
                        }
                    }
                }
            }
            if all.is_empty() {
                break;
            }
            let edits = all;
            let next = apply(&ed, &edits);
            if next == ed {
                break;
            }
            ed = next;
            steps += 1;
        }
        if cli == text && ed == text {
            nofix += 1;
            continue;
        }
        if cli == ed {
            same += 1;
            continue;
        }
        differ += 1;
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let outd = std::path::Path::new(&dir).parent().unwrap().join("miss");
        std::fs::create_dir_all(&outd).unwrap();
        std::fs::write(outd.join(format!("{name}.orig")), &text).unwrap();
        std::fs::write(outd.join(format!("{name}.cli")), &cli).unwrap();
        std::fs::write(outd.join(format!("{name}.lsp")), &ed).unwrap();
        eprintln!("MISS {name} (editor steps {steps})");
    }
    eprintln!("PARITY same={same} differ={differ} nofix={nofix}");
}

#[test]
#[ignore]
#[allow(clippy::mutable_key_type)]
fn first_pass() {
    let dir = std::env::var("KARA_PARITY_DIR").unwrap();
    let out = std::env::var("KARA_PARITY_OUT").unwrap();
    std::fs::create_dir_all(&out).unwrap();
    for f in std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()) {
        let text = std::fs::read_to_string(&f).unwrap();
        let p = std::env::temp_dir().join("kara-parity-work/main.kara");
        let uri = Uri::from_str(&format!("file://{}", p.display())).unwrap();
        let acts = code_actions(
            &text,
            &uri,
            Range::new(Position::new(0, 0), Position::new(99999, 0)),
        );
        let mut lines = Vec::new();
        for a in acts {
            if let CodeActionOrCommand::CodeAction(a) = a {
                if a.kind != Some(CodeActionKind::QUICKFIX) {
                    continue;
                }
                let edits = a
                    .edit
                    .unwrap()
                    .changes
                    .unwrap()
                    .into_values()
                    .next()
                    .unwrap();
                for e in edits {
                    let (s, t) = (offset(&text, e.range.start), offset(&text, e.range.end));
                    let line_start = text[..s].rfind('\n').map_or(0, |i| i + 1);
                    let line = text[..s].matches('\n').count() + 1;
                    let col = s - line_start + 1;
                    lines.push(format!(
                        "{}\t{}\t{:?}\t{:?}",
                        line,
                        col,
                        &text[s..t],
                        e.new_text
                    ));
                }
            }
        }
        std::fs::write(
            std::path::Path::new(&out)
                .join(format!("{}.lsp", f.file_name().unwrap().to_string_lossy())),
            lines.join("\n"),
        )
        .unwrap();
    }
}
