mod common;
mod html;
mod markdown;
mod pdf;
mod router;
mod text;

pub use html::HtmlParser;
pub use markdown::MarkdownParser;
pub use pdf::PdfParser;
pub use router::ParserRouter;
pub use text::TextParser;
