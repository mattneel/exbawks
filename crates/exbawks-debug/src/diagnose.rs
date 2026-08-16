//! Ariadne-rendered diagnosis of a decoded guest region.
//!
//! When a run stops at an unimplemented surface element, the application
//! disassembles the bytes around the stop site and highlights the offending
//! instruction or call, so the wall is a labeled source annotation rather
//! than a bare address.

use ariadne::{Color, Config, Label, Report, ReportKind, Source};

/// Renders a decoded region as an ariadne report highlighting one line.
///
/// `lines` are the formatted instructions of the region in order;
/// `highlight` selects the line to label. Returns rendered text (with ANSI
/// color) suitable for printing to a terminal.
#[must_use]
pub fn render_site(
    title: &str,
    lines: &[String],
    highlight: usize,
    label: &str,
    note: Option<&str>,
) -> String {
    const ID: &str = "guest";

    let source_text = lines.join("\n");
    let mut offset = 0_usize;
    let mut span = 0..source_text.len();
    for (index, line) in lines.iter().enumerate() {
        if index == highlight {
            span = offset..offset + line.len();
            break;
        }
        offset += line.len() + 1;
    }

    let mut builder = Report::build(ReportKind::Error, ID, span.start)
        .with_config(Config::default())
        .with_message(title.to_owned())
        .with_label(Label::new((ID, span)).with_message(label.to_owned()).with_color(Color::Red));
    if let Some(note) = note {
        builder = builder.with_note(note.to_owned());
    }

    let mut buffer = Vec::new();
    if builder.finish().write((ID, Source::from(source_text)), &mut buffer).is_err() {
        return String::new();
    }
    String::from_utf8(buffer).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_highlighted_line() {
        let lines = vec![
            "0x1000  mov ecx, [0x10118]".to_owned(),
            "0x1006  call [0x11200]".to_owned(),
            "0x100c  ret".to_owned(),
        ];
        let rendered = render_site(
            "unimplemented kernel export",
            &lines,
            1,
            "calls KeSetEvent (ordinal 145)",
            Some("1 of 152 imports missing"),
        );
        assert!(rendered.contains("call [0x11200]"), "the highlighted source must appear");
        assert!(rendered.contains("KeSetEvent"), "the label must appear");
    }
}
