use super::*;

fn plain_text(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn renders_common_agent_markdown() {
    let lines = render(
        "# Result\n\n- **bold** and _emphasis_ and `code`\n\n| A | B |\n|---|---|\n| 1 | 2 |",
        crate::frontend::theme::current().style(crate::frontend::theme::Role::Text),
    );
    let text = plain_text(&lines);
    let modifiers = lines
        .iter()
        .flat_map(|line| &line.spans)
        .map(|span| span.style.add_modifier)
        .collect::<Vec<_>>();

    assert!(text.contains("Result") && text.contains("- bold and emphasis and code"));
    assert!(text.contains("A │ B") && text.contains("1 │ 2"));
    assert!(modifiers.iter().any(|value| value.contains(Modifier::BOLD)));
    assert!(
        modifiers
            .iter()
            .any(|value| value.contains(Modifier::ITALIC))
    );
}
