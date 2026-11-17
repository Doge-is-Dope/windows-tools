/// How much of what a command printed is kept: the end, where the errors are.
pub const KEPT: usize = 64 * 1024;

/// What a command printed, stdout and stderr in the order they arrived, keeping only the last
/// `limit` bytes.
pub struct Tail {
    text: String,
    limit: usize,
    cut: bool,
}
impl Tail {
    pub fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            limit,
            cut: false,
        }
    }
    pub fn push(&mut self, text: &str) {
        self.text.push_str(text);
        // Trimmed in batches, so a long run does not move the kept text on every write.
        if self.text.len() > 2 * self.limit {
            self.trim();
        }
    }
    /// The kept text, cut on a character boundary, and whether anything was cut.
    pub fn end(&mut self) -> (String, bool) {
        self.trim();
        (self.text.clone(), self.cut)
    }
    fn trim(&mut self) {
        if self.text.len() <= self.limit {
            return;
        }
        let mut start = self.text.len() - self.limit;
        while !self.text.is_char_boundary(start) {
            start += 1;
        }
        self.text.drain(..start);
        self.cut = true;
    }
}
#[derive(Default)]
pub struct Utf8Stream {
    pending: Vec<u8>,
}
impl Utf8Stream {
    pub fn push(&mut self, bytes: &[u8], end: bool) -> String {
        self.pending.extend_from_slice(bytes);
        let mut text = String::new();
        let mut offset = 0;
        loop {
            match std::str::from_utf8(&self.pending[offset..]) {
                Ok(valid) => {
                    text.push_str(valid);
                    offset = self.pending.len();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    text.push_str(
                        std::str::from_utf8(&self.pending[offset..offset + valid]).unwrap(),
                    );
                    offset += valid;
                    if let Some(length) = error.error_len() {
                        text.push('\u{fffd}');
                        offset += length;
                    } else if end {
                        text.push('\u{fffd}');
                        offset = self.pending.len();
                        break;
                    } else {
                        break;
                    }
                }
            }
        }
        self.pending.drain(..offset);
        text
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_utf8_is_preserved() {
        let mut d = Utf8Stream::default();
        let bytes = "中文🙂".as_bytes();
        let mut s = String::new();
        for b in bytes {
            s.push_str(&d.push(&[*b], false));
        }
        s.push_str(&d.push(&[], true));
        assert_eq!(s, "中文🙂");
    }
    #[test]
    fn the_end_of_what_was_printed_is_kept_in_order() {
        let mut tail = Tail::new(1024);
        tail.push("out 1\n");
        tail.push("error 長\n");
        tail.push("out 2\n");
        assert_eq!(tail.end(), ("out 1\nerror 長\nout 2\n".into(), false));
        // Cut on a character boundary, never inside one.
        let mut tail = Tail::new(9);
        tail.push("out 1\nerror 長\nout 2\n");
        assert_eq!(tail.end(), ("\nout 2\n".into(), true));
    }
    #[test]
    fn output_far_past_the_limit_still_keeps_its_end() {
        let mut tail = Tail::new(KEPT);
        for _ in 0..100 {
            tail.push(&"x".repeat(KEPT / 4));
        }
        tail.push("THE END");
        let (text, cut) = tail.end();
        assert!(cut && text.len() == KEPT && text.ends_with("xTHE END"));
    }
}
