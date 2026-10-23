//! JSON text without the capacity slack `serde_json::to_string` leaves.
//!
//! `serde_json::to_string` grows its buffer by doubling, so a 19 MB value can end up in a 32 MiB
//! allocation. A document's metadata values (the normalized block map of a book is the largest)
//! live for the whole open, so that slack is part of the open's peak for as long as the document
//! does: for 宋史 13 MiB of a 92 MiB heap peak. [`to_string_exact`] returns the same bytes in a
//! `String` whose capacity is exactly its length.

use std::io;

use serde::Serialize;

/// A writer that keeps nothing but the length of what it is given.
struct CountingWriter(usize);

impl io::Write for CountingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0 += buffer.len();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The JSON text of `value`, in a `String` whose capacity is exactly its length.
///
/// The first pass serialises into [`CountingWriter`], which allocates nothing, to learn the exact
/// length; the second serialises into a `Vec<u8>` reserved once for that length, so the buffer is
/// never regrown and no larger buffer is ever live. `String::from_utf8` keeps the same allocation,
/// so the result is the text and nothing else. The bytes are `serde_json::to_string`'s byte for
/// byte; `tests/json_exact_capacity.rs` asserts it.
///
/// The value is serialised twice, so this is for the large values a document keeps: a small one
/// costs more in the second pass than the slack it saves.
pub fn to_string_exact<T>(value: &T) -> Result<String, serde_json::Error>
where
    T: Serialize + ?Sized,
{
    let mut counter = CountingWriter(0);
    serde_json::to_writer(&mut counter, value)?;
    let mut bytes = Vec::with_capacity(counter.0);
    serde_json::to_writer(&mut bytes, value)?;
    debug_assert_eq!(bytes.len(), counter.0);
    String::from_utf8(bytes).map_err(|error| {
        // serde_json only ever emits UTF-8, so this is unreachable; it is an error rather than a
        // panic so a bug can never take the server down.
        serde_json::Error::io(io::Error::new(io::ErrorKind::InvalidData, error))
    })
}
