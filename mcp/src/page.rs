// SPDX-License-Identifier: Apache-2.0

//! One page of a library scan.
//!
//! The page holds the matches the caller asked for. It does not hold the
//! rest of the library. `limit` is that page size. There is no maximum.

use crate::metadata::Record;

#[derive(Debug, Clone)]
pub struct Hit {
    pub path: String,
    pub bytes: u64,
    pub record: Option<Record>,
}

#[derive(Debug, Default)]
pub struct Page {
    pub hits: Vec<Hit>,
    pub truncated: bool,
}

impl Page {
    /// Store `hit` when the page still has room.
    ///
    /// Returns true when the scan should stop. A full page does not store
    /// this hit; the caller resumes after the last stored path.
    pub fn consider(&mut self, limit: u64, hit: Hit) -> bool {
        if self.hits.len() as u64 >= limit {
            self.truncated = true;
            return true;
        }
        self.hits.push(hit);
        false
    }

    pub fn next_cursor(&self) -> Option<&str> {
        if self.truncated {
            self.hits.last().map(|hit| hit.path.as_str())
        } else {
            None
        }
    }
}

/// Counted library used by the scale test. Paths and captions are computed
/// from the index. Nothing is stored for an item the page does not return.
#[derive(Debug, Clone, Copy)]
pub struct SyntheticLibrary {
    pub items: u64,
    /// Items whose index is a multiple of this value carry `scale-caption`.
    /// Zero means no item has a caption.
    pub caption_stride: u64,
}

impl SyntheticLibrary {
    pub fn path_at(index: u64) -> String {
        format!("i/{index:08}.jpg")
    }

    pub fn index_of(path: &str) -> Option<u64> {
        let rest = path.strip_prefix("i/")?.strip_suffix(".jpg")?;
        if rest.len() != 8 || !rest.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        rest.parse().ok()
    }

    pub fn record_at(self, index: u64) -> Record {
        let mut record = Record {
            captured: None,
            camera: None,
            lens: None,
            latitude: None,
            longitude: None,
            keywords: Vec::new(),
            captions: Vec::new(),
        };
        if self.caption_stride > 0 && index.is_multiple_of(self.caption_stride) {
            record.captions.push("scale-caption".to_string());
        }
        record
    }

    /// `query` is matched against the relative path. `caption` is matched
    /// against the synthetic caption. Both are case-insensitive substrings.
    pub fn page(
        self,
        query: Option<&str>,
        caption: Option<&str>,
        after: Option<&str>,
        limit: u64,
    ) -> Page {
        let query = query.map(str::to_lowercase);
        let caption = caption.map(str::to_lowercase);
        let mut index = after
            .and_then(Self::index_of)
            .map(|found| found.saturating_add(1))
            .unwrap_or(0);
        let mut page = Page::default();
        while index < self.items {
            let path = Self::path_at(index);
            if let Some(after) = after {
                if path.as_str() <= after {
                    index += 1;
                    continue;
                }
            }
            let record = self.record_at(index);
            let query_ok = query
                .as_ref()
                .map(|query| path.to_lowercase().contains(query))
                .unwrap_or(true);
            let caption_ok = caption
                .as_ref()
                .map(|caption| {
                    record
                        .captions
                        .iter()
                        .any(|item| item.to_lowercase().contains(caption))
                })
                .unwrap_or(true);
            index += 1;
            if !query_ok || !caption_ok {
                continue;
            }
            if page.consider(
                limit,
                Hit {
                    path,
                    bytes: 0,
                    record: Some(record),
                },
            ) {
                break;
            }
        }
        page
    }
}

pub fn rss_bytes() -> u64 {
    let text = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("VmRSS:") else {
            continue;
        };
        let Some(kb) = rest.split_whitespace().next() else {
            continue;
        };
        if let Ok(kb) = kb.parse::<u64>() {
            return kb.saturating_mul(1024);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn million_item_caption_pages_are_complete() {
        let library = SyntheticLibrary {
            items: 1_000_000,
            caption_stride: 1_000,
        };
        let mut cursor = None;
        let mut seen = Vec::new();
        let mut pages = 0u32;
        loop {
            let page = library.page(None, Some("scale-caption"), cursor.as_deref(), 64);
            assert!(page.hits.len() <= 64);
            pages += 1;
            assert!(pages < 100, "pagination did not finish");
            for hit in &page.hits {
                let index = SyntheticLibrary::index_of(&hit.path).unwrap();
                assert_eq!(index % 1_000, 0, "{}", hit.path);
                assert_eq!(
                    hit.record.as_ref().unwrap().captions,
                    vec!["scale-caption".to_string()]
                );
                seen.push(hit.path.clone());
            }
            if !page.truncated {
                assert!(page.next_cursor().is_none());
                break;
            }
            let next = page.next_cursor().unwrap().to_string();
            assert_ne!(cursor.as_deref(), Some(next.as_str()));
            cursor = Some(next);
        }
        assert_eq!(seen.len(), 1_000);
        let mut unique = seen.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique, seen);
        assert_eq!(seen.first().map(String::as_str), Some("i/00000000.jpg"));
        assert_eq!(seen.last().map(String::as_str), Some("i/00999000.jpg"));
    }

    #[test]
    fn a_path_query_finds_one_item_in_a_million() {
        let library = SyntheticLibrary {
            items: 1_000_000,
            caption_stride: 0,
        };
        let page = library.page(Some("i/00000042.jpg"), None, None, 10);
        assert_eq!(page.hits.len(), 1);
        assert!(!page.truncated);
        assert_eq!(page.hits[0].path, "i/00000042.jpg");
        assert!(page.hits[0].record.as_ref().unwrap().captions.is_empty());
    }

    #[test]
    fn memory_stays_flat_from_100k_to_1m() {
        let small = SyntheticLibrary {
            items: 100_000,
            caption_stride: 0,
        };
        let large = SyntheticLibrary {
            items: 1_000_000,
            caption_stride: 0,
        };
        // A query that matches every path, with a small page. Collecting the
        // library before truncating would retain every path.
        let first = small.page(Some("jpg"), None, None, 32);
        assert_eq!(first.hits.len(), 32);
        assert!(first.truncated);
        let rss_small = rss_bytes();
        let second = large.page(Some("jpg"), None, None, 32);
        assert_eq!(second.hits.len(), 32);
        assert!(second.truncated);
        assert_eq!(second.next_cursor(), Some("i/00000031.jpg"));
        let rss_large = rss_bytes();
        let slack = 32 * 1024 * 1024;
        assert!(
            rss_large <= rss_small.saturating_add(slack),
            "rss grew from {rss_small} to {rss_large}"
        );
    }
}
