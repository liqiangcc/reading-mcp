mod adaptive_search;
mod budget;
mod cache;
mod file_cache;
mod memory_index;
mod memory_repository;
mod noop_index;
mod observability;
mod sqlite;

pub use adaptive_search::AdaptiveSearchIndex;
pub use budget::{BlockingParser, BudgetedParser, BudgetedRetriever, ResourceBudget};
pub use cache::{
    CachingParser, CachingRetriever, InMemoryParsedDocumentCache, InMemoryRawResourceCache,
};
pub use file_cache::{FileParsedDocumentCache, FileRawResourceCache};
pub use memory_index::InMemorySearchIndex;
pub use memory_repository::InMemoryDocumentRepository;
pub use noop_index::NoopSearchIndex;
pub use observability::{
    ObservedParsedDocumentCache, ObservedParser, ObservedRawResourceCache, ObservedRetriever,
    ObservedSearchIndex,
};
pub use sqlite::{SqliteDocumentRepository, SqliteSearchIndex};
