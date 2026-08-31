use std::io::Cursor;

use image::{ImageFormat, RgbaImage};
use pdf_render::pdf_interpret::InterpreterSettings;
use pdf_render::pdf_syntax::{Pdf, PdfLoadLimits};

use crate::application::ports::{
    ApplicationError, RenderedSourceView, SourceViewRenderOptions, SourceViewRenderer,
};
use crate::domain::MediaType;

#[derive(Default)]
pub struct PdfSourceViewRenderer;

impl SourceViewRenderer for PdfSourceViewRenderer {
    fn render(
        &self,
        bytes: Vec<u8>,
        media_type: MediaType,
        page: u32,
        options: SourceViewRenderOptions,
    ) -> Result<RenderedSourceView, ApplicationError> {
        if !is_pdf(&media_type) {
            return Err(ApplicationError::SourceViewFailed(format!(
                "unsupported source-view media type {}",
                media_type.0
            )));
        }

        let load_limits = PdfLoadLimits::new()
            .max_object_depth(64)
            .max_image_pixels(options.max_pixels)
            .max_stream_bytes(options.max_decoded_stream_bytes);
        let pdf = Pdf::new_with_limits(bytes, load_limits).map_err(|error| {
            ApplicationError::SourceViewFailed(format!("invalid PDF source view: {error:?}"))
        })?;
        let pages = pdf.pages();
        let page_count = pages.len();
        if page_count > options.max_pages {
            return Err(ApplicationError::ResourceLimitExceeded(format!(
                "PDF has {page_count} pages; source-view limit is {} pages",
                options.max_pages
            )));
        }

        let page_index = page
            .checked_sub(1)
            .and_then(|value| usize::try_from(value).ok());
        let page_index = page_index.ok_or_else(|| {
            ApplicationError::InvalidRequest("source-view page must be 1-based".into())
        })?;
        let source_page = pages.get(page_index).ok_or_else(|| {
            ApplicationError::InvalidRequest(format!(
                "source-view page {page} is outside the PDF page range 1..={page_count}"
            ))
        })?;

        let (base_width, base_height) = source_page.render_dimensions();
        let scale = options.dpi as f32 / 72.0;
        let width = checked_dimension(base_width, scale, options.max_width, "width")?;
        let height = checked_dimension(base_height, scale, options.max_height, "height")?;
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or_else(|| {
                ApplicationError::ResourceLimitExceeded("source-view pixel count overflowed".into())
            })?;
        if pixels > options.max_pixels {
            return Err(ApplicationError::ResourceLimitExceeded(format!(
                "source-view image has {pixels} pixels; limit is {}",
                options.max_pixels
            )));
        }

        let pixmap = pdf_render::render(
            source_page,
            &InterpreterSettings::default(),
            &pdf_render::RenderSettings {
                x_scale: scale,
                y_scale: scale,
                width: Some(u16::try_from(width).expect("checked source-view width")),
                height: Some(u16::try_from(height).expect("checked source-view height")),
                bg_color: pdf_render::vello_cpu::color::palette::css::WHITE,
                ..Default::default()
            },
        );

        let image = RgbaImage::from_raw(
            u32::from(pixmap.width()),
            u32::from(pixmap.height()),
            pixmap.data_as_u8_slice().to_vec(),
        )
        .ok_or_else(|| {
            ApplicationError::SourceViewFailed(
                "PDF renderer returned an invalid RGBA image buffer".into(),
            )
        })?;
        let mut encoded = Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, ImageFormat::Png)
            .map_err(|error| {
                ApplicationError::SourceViewFailed(format!(
                    "failed to encode source-view PNG: {error}"
                ))
            })?;
        let encoded = encoded.into_inner();
        if encoded.len() > options.max_image_bytes {
            return Err(ApplicationError::ResourceLimitExceeded(format!(
                "source-view image is {} bytes; limit is {} bytes",
                encoded.len(),
                options.max_image_bytes
            )));
        }

        Ok(RenderedSourceView {
            media_type: MediaType("image/png".into()),
            bytes: encoded,
            width: u32::from(pixmap.width()),
            height: u32::from(pixmap.height()),
            page_count,
        })
    }
}

fn checked_dimension(
    base: f32,
    scale: f32,
    limit: u32,
    label: &str,
) -> Result<u32, ApplicationError> {
    let scaled = f64::from(base) * f64::from(scale);
    if !scaled.is_finite() || scaled <= 0.0 {
        return Err(ApplicationError::SourceViewFailed(format!(
            "PDF page has invalid {label} dimension"
        )));
    }
    let dimension = scaled.round();
    if dimension < 1.0 {
        return Err(ApplicationError::SourceViewFailed(format!(
            "PDF page has an unusable {label} dimension"
        )));
    }
    if dimension > f64::from(limit) {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "source-view image {label} is {dimension:.0}px; limit is {limit}px"
        )));
    }
    if dimension > f64::from(u16::MAX) {
        return Err(ApplicationError::ResourceLimitExceeded(format!(
            "source-view image {label} exceeds renderer dimensions"
        )));
    }
    u32::try_from(dimension as u64).map_err(|_| {
        ApplicationError::ResourceLimitExceeded(format!(
            "source-view image {label} exceeds supported dimensions"
        ))
    })
}

fn is_pdf(media_type: &MediaType) -> bool {
    media_type
        .0
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/pdf"))
}
