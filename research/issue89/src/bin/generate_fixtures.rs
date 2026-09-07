use std::env;
use std::fs;
use std::path::PathBuf;

use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, Stream, dictionary};

#[derive(Clone, Copy)]
struct Line<'a> {
    x: i64,
    y: i64,
    text: &'a str,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env::args().nth(1).ok_or("output directory required")?);
    fs::create_dir_all(&root)?;
    write_pdf(&root.join("single-column.pdf"), &[&[
        Line { x: 72, y: 810, text: "Abstract" },
        Line { x: 72, y: 790, text: "The abstract has one complete sentence." },
        Line { x: 72, y: 770, text: "A second abstract sentence follows." },
        Line { x: 72, y: 740, text: "1 Introduction" },
        Line { x: 72, y: 720, text: "A normal paragraph has one complete sentence." },
        Line { x: 72, y: 700, text: "A second sentence follows across the same column." },
    ]])?;
    write_pdf(&root.join("double-column.pdf"), &[&[
        Line { x: 72, y: 810, text: "Abstract" },
        Line { x: 72, y: 790, text: "The left abstract sentence is complete." },
        Line { x: 72, y: 770, text: "The second abstract sentence is complete." },
        Line { x: 72, y: 750, text: "The third abstract sentence is complete." },
        Line { x: 72, y: 730, text: "The fourth abstract sentence is complete." },
        Line { x: 72, y: 700, text: "1 Left heading" },
        Line { x: 72, y: 680, text: "Left column first sentence." },
        Line { x: 72, y: 660, text: "Left column second sentence." },
        Line { x: 300, y: 700, text: "Right column first sentence." },
        Line { x: 300, y: 680, text: "Right column second sentence." },
    ]])?;
    write_pdf(&root.join("cross-page.pdf"), &[
        &[
            Line { x: 72, y: 810, text: "Abstract" },
            Line { x: 72, y: 790, text: "The abstract first sentence is complete." },
            Line { x: 72, y: 770, text: "The abstract second sentence is complete." },
            Line { x: 72, y: 740, text: "1 Cross-page heading" },
            Line { x: 72, y: 720, text: "The first sentence ends on this page" },
        ],
        &[
            Line { x: 72, y: 760, text: "and continues on the next page." },
            Line { x: 72, y: 740, text: "A final sentence ends here." },
        ],
    ])?;
    write_pdf(&root.join("hyphenated.pdf"), &[&[
        Line { x: 72, y: 810, text: "Abstract" },
        Line { x: 72, y: 790, text: "The abstract first sentence is complete." },
        Line { x: 72, y: 770, text: "The abstract second sentence is complete." },
        Line { x: 72, y: 740, text: "1 Hyphenation" },
        Line { x: 72, y: 720, text: "A cross-" },
        Line { x: 72, y: 700, text: "page word should rejoin." },
        Line { x: 72, y: 680, text: "The next sentence is separate." },
    ]])?;
    write_pdf(&root.join("space-loss.pdf"), &[&[
        Line { x: 72, y: 810, text: "Abstract" },
        Line { x: 72, y: 790, text: "The abstract sentence is complete." },
        Line { x: 72, y: 760, text: "1 Spacing" },
        Line { x: 72, y: 740, text: "Metadata and" },
        Line { x: 145, y: 740, text: "loss can remove a boundary." },
    ]])?;
    write_pdf(&root.join("punctuation-identifiers.pdf"), &[&[
        Line { x: 72, y: 810, text: "Abstract" },
        Line { x: 72, y: 790, text: "The abstract first sentence is complete." },
        Line { x: 72, y: 770, text: "The abstract second sentence is complete." },
        Line { x: 72, y: 740, text: "1 Technical text" },
        Line { x: 72, y: 720, text: "Use e.g. a value such as 3.14 in RFC-1234." },
        Line { x: 72, y: 700, text: "This is a second sentence." },
    ]])?;
    Ok(())
}

fn write_pdf(path: &PathBuf, pages: &[&[Line<'_>]]) -> Result<(), Box<dyn std::error::Error>> {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
    let resources_id = document.add_object(dictionary! { "Font" => dictionary! { "F1" => font_id } });
    let mut page_ids = Vec::new();
    for page in pages {
        let operations = page.iter().flat_map(|line| vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 10.into()]),
            Operation::new("Tm", vec![1.into(), 0.into(), 0.into(), 1.into(), line.x.into(), line.y.into()]),
            Operation::new("Tj", vec![Object::string_literal(line.text)]),
            Operation::new("ET", vec![]),
        ]).collect::<Vec<_>>();
        let content_id = document.add_object(Stream::new(dictionary! {}, Content { operations }.encode()?));
        page_ids.push(document.add_object(dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => content_id }));
    }
    document.objects.insert(pages_id, Object::Dictionary(dictionary! {
        "Type" => "Pages", "Kids" => page_ids.iter().copied().map(Object::Reference).collect::<Vec<_>>(),
        "Count" => page_ids.len() as i64, "Resources" => resources_id, "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    }));
    let catalog_id = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    document.trailer.set("Root", catalog_id);
    document.save(path)?;
    Ok(())
}
