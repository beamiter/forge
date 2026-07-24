use std::collections::VecDeque;

/// A lazily allocated byte ring that retains only the newest `limit` bytes.
///
/// Appending after the ring reaches its limit advances the front instead of
/// shifting the retained tail in memory. A single oversized append bypasses
/// the existing contents and copies only the portion that can be retained.
#[derive(Debug)]
pub(super) struct BoundedByteRing {
    bytes: VecDeque<u8>,
    limit: usize,
}

impl BoundedByteRing {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            bytes: VecDeque::new(),
            limit,
        }
    }

    pub(super) fn append(&mut self, input: &[u8]) {
        if input.is_empty() || self.limit == 0 {
            return;
        }

        if input.len() >= self.limit {
            self.bytes.clear();
            self.bytes
                .extend(input[input.len() - self.limit..].iter().copied());
            return;
        }

        let overflow = self
            .bytes
            .len()
            .saturating_add(input.len())
            .saturating_sub(self.limit);
        if overflow > 0 {
            self.bytes.drain(..overflow);
        }
        self.bytes.extend(input.iter().copied());
    }

    pub(super) fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.bytes.clear();
    }

    /// Make the retained tail contiguous for one final linear read.
    ///
    /// This may rearrange the ring once, when a command is finalized, but does
    /// not allocate a second byte buffer.
    pub(super) fn make_contiguous(&mut self) -> &[u8] {
        self.bytes.make_contiguous()
    }

    #[cfg(test)]
    fn to_vec(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(self.bytes.len());
        let (head, tail) = self.bytes.as_slices();
        output.extend_from_slice(head);
        output.extend_from_slice(tail);
        output
    }

    pub(super) fn take_vec(&mut self) -> Vec<u8> {
        let bytes = std::mem::take(&mut self.bytes);
        bytes.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::BoundedByteRing;

    #[test]
    fn retains_input_at_and_below_the_limit() {
        let mut ring = BoundedByteRing::new(5);
        ring.append(b"abc");
        ring.append(b"");
        assert_eq!(ring.to_vec(), b"abc");

        ring.append(b"de");
        assert_eq!(ring.to_vec(), b"abcde");
    }

    #[test]
    fn wrapped_appends_retain_the_newest_bytes_in_order() {
        let mut ring = BoundedByteRing::new(5);
        ring.append(b"abcd");
        ring.append(b"ef");
        assert_eq!(ring.to_vec(), b"bcdef");

        ring.append(b"gh");
        assert_eq!(ring.to_vec(), b"defgh");
        assert_eq!(ring.make_contiguous(), b"defgh");
    }

    #[test]
    fn oversized_append_copies_only_its_tail() {
        let mut ring = BoundedByteRing::new(5);
        ring.append(b"old");
        ring.append(b"0123456789");
        assert_eq!(ring.to_vec(), b"56789");

        ring.append(b"XY");
        assert_eq!(ring.to_vec(), b"789XY");
    }

    #[test]
    fn clear_and_take_empty_the_ring_for_reuse() {
        let mut ring = BoundedByteRing::new(4);
        ring.append(b"abcdef");
        ring.clear();
        assert!(ring.is_empty());

        ring.append(b"12");
        assert_eq!(ring.take_vec(), b"12");
        assert!(ring.is_empty());

        ring.append(b"34567");
        assert_eq!(ring.to_vec(), b"4567");
    }

    #[test]
    fn zero_limit_never_retains_bytes() {
        let mut ring = BoundedByteRing::new(0);
        ring.append(b"ignored");
        assert!(ring.is_empty());
        assert!(ring.take_vec().is_empty());
    }
}
