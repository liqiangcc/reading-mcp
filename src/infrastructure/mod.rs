mod budget;
mod cache;
mod file_cache;
mod lexical;
mod memory_index;
mod memory_repository;
mod memory_text_unit_index;
mod noop_index;
mod observability;
mod ocr_evidence;
mod ocr_identity;
mod ocr_identity_process;
mod ocr_singleflight;
mod sqlite;
mod sqlite_search_index;

pub use budget::{BudgetedParser, BudgetedRetriever, ResourceBudget};
pub use cache::{
    CachingParser, CachingRetriever, InMemoryParsedDocumentCache, InMemoryRawResourceCache,
};
pub use file_cache::{FileParsedDocumentCache, FileRawResourceCache};
pub use memory_index::InMemorySearchIndex;
pub use memory_repository::InMemoryDocumentRepository;
pub use memory_text_unit_index::InMemoryTextUnitIndex;
pub use noop_index::NoopSearchIndex;
pub use observability::{
    ObservedParsedDocumentCache, ObservedParser, ObservedRawResourceCache, ObservedRetriever,
    ObservedSearchIndex,
};
pub use ocr_evidence::FileOcrEvidenceStore;
pub(crate) use ocr_identity::OCR_PROCESS_ENV;
pub use ocr_identity::build_ocr_runtime_identity;
pub(crate) use ocr_identity_process::dependency_output;
#[doc(hidden)]
pub use sqlite::SqliteSearchIndex as LegacySqliteSearchIndex;
pub use sqlite::{SqliteDocumentRepository, SqliteTextUnitIndex};
pub use sqlite_search_index::SqliteSearchIndex;
