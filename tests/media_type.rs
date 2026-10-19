use reading_mcp::domain::MediaType;

fn media(value: &str) -> MediaType {
    MediaType(value.into())
}

#[test]
fn essence_drops_parameters_and_whitespace() {
    assert_eq!(media("text/html; charset=utf-8").essence(), "text/html");
    assert_eq!(media("  application/pdf  ").essence(), "application/pdf");
    assert_eq!(media("").essence(), "");
}

#[test]
fn is_pdf_ignores_case_and_parameters() {
    assert!(media("application/pdf").is_pdf());
    assert!(media("Application/PDF; charset=binary").is_pdf());
    assert!(!media("application/pdfx").is_pdf());
    assert!(!media("text/plain").is_pdf());
}
