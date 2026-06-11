//! Pagination input for list endpoints.
//!
//! Offset-based pagination keeps the trait simple: every backend can apply
//! `LIMIT/OFFSET` (or its equivalent) without invent ing a cursor format.
//! Cursor-based variants can layer on top later without breaking the trait.

/// Pagination request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    pub limit: usize,
    pub offset: usize,
}

impl Page {
    /// Construct a `Page` requesting the first `limit` items.
    #[must_use]
    pub const fn first(limit: usize) -> Self {
        Self { limit, offset: 0 }
    }

    /// Construct a `Page` with explicit limit and offset.
    #[must_use]
    pub const fn new(limit: usize, offset: usize) -> Self {
        Self { limit, offset }
    }

    /// Advance the page by `limit` items. Returns the next-page request.
    #[must_use]
    pub const fn next(self) -> Self {
        Self {
            limit: self.limit,
            offset: self.offset + self.limit,
        }
    }
}

impl Default for Page {
    /// Default page is `limit = 50, offset = 0`.
    fn default() -> Self {
        Self::first(50)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_starts_at_offset_zero() {
        let p = Page::first(10);
        assert_eq!(p.limit, 10);
        assert_eq!(p.offset, 0);
    }

    #[test]
    fn next_advances_by_limit() {
        let p = Page::first(20);
        let p2 = p.next();
        assert_eq!(p2.limit, 20);
        assert_eq!(p2.offset, 20);
        let p3 = p2.next();
        assert_eq!(p3.offset, 40);
    }

    #[test]
    fn explicit_new_keeps_values() {
        let p = Page::new(5, 25);
        assert_eq!(p.limit, 5);
        assert_eq!(p.offset, 25);
    }

    #[test]
    fn default_is_first_50() {
        let p = Page::default();
        assert_eq!(p.limit, 50);
        assert_eq!(p.offset, 0);
    }
}
