//! What a person types in the coding screen: a few lines of text with a cursor, and the
//! prompts sent before, to bring back. Pure: keys are turned into these moves elsewhere.

/// How many columns a tab typed or pasted takes.
const TAB: &str = "    ";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Composer {
    /// The text by character, so the cursor never lands inside one.
    text: Vec<char>,
    /// Where the next character goes: `0..=text.len()`.
    cursor: usize,
    /// Prompts sent, oldest first.
    history: Vec<String>,
    /// Which of them is shown while going back; `None`: what is being typed.
    recalled: Option<usize>,
    /// What was being typed before going back.
    draft: String,
}

/// The text as rows no wider than a width, and where the cursor is in them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rows {
    pub rows: Vec<String>,
    /// Row and column, from 0.
    pub cursor: (usize, usize),
}

impl Composer {
    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Nothing but spaces and new lines: nothing to send.
    pub fn is_blank(&self) -> bool {
        self.text.iter().all(|c| c.is_whitespace())
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.recalled = None;
    }

    pub fn set(&mut self, text: &str) {
        self.text = text.chars().collect();
        self.cursor = self.text.len();
    }

    pub fn insert(&mut self, c: char) {
        match c {
            '\t' => self.insert_str(TAB),
            '\n' => self.put('\n'),
            c if c.is_control() => {}
            c => self.put(c),
        }
    }

    /// Text that arrives at once (a paste): line ends become new lines, tabs spaces, and
    /// what would move a terminal's cursor is dropped.
    pub fn insert_str(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        for c in text.chars() {
            match c {
                '\t' => TAB.chars().for_each(|c| self.put(c)),
                '\n' => self.put('\n'),
                c if c.is_control() => {}
                c => self.put(c),
            }
        }
    }

    fn put(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += 1;
        self.recalled = None;
    }

    pub fn newline(&mut self) {
        self.put('\n');
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.text.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.len());
    }

    /// Where the line the cursor is on starts and ends (before its new line).
    fn line(&self) -> (usize, usize) {
        let start = self.text[..self.cursor]
            .iter()
            .rposition(|c| *c == '\n')
            .map_or(0, |at| at + 1);
        let end = self.text[self.cursor..]
            .iter()
            .position(|c| *c == '\n')
            .map_or(self.text.len(), |at| self.cursor + at);
        (start, end)
    }

    pub fn home(&mut self) {
        self.cursor = self.line().0;
    }

    pub fn end(&mut self) {
        self.cursor = self.line().1;
    }

    /// One line up, keeping the column. `false`: it is on the first line already.
    pub fn up(&mut self) -> bool {
        let (start, _) = self.line();
        if start == 0 {
            return false;
        }
        let column = self.cursor - start;
        self.cursor = start - 1;
        let (above, end) = self.line();
        self.cursor = (above + column).min(end);
        true
    }

    /// One line down, keeping the column. `false`: it is on the last line already.
    pub fn down(&mut self) -> bool {
        let (start, end) = self.line();
        if end == self.text.len() {
            return false;
        }
        let column = self.cursor - start;
        self.cursor = end + 1;
        let (below, end) = self.line();
        self.cursor = (below + column).min(end);
        true
    }

    pub fn word_left(&mut self) {
        while self.cursor > 0 && self.text[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
        }
        while self.cursor > 0 && !self.text[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
        }
    }

    pub fn word_right(&mut self) {
        let len = self.text.len();
        while self.cursor < len && self.text[self.cursor].is_whitespace() {
            self.cursor += 1;
        }
        while self.cursor < len && !self.text[self.cursor].is_whitespace() {
            self.cursor += 1;
        }
    }

    /// Deletes the word before the cursor.
    pub fn delete_word(&mut self) {
        let end = self.cursor;
        self.word_left();
        self.text.drain(self.cursor..end);
    }

    /// Deletes from the start of the line to the cursor.
    pub fn delete_to_home(&mut self) {
        let start = self.line().0;
        self.text.drain(start..self.cursor);
        self.cursor = start;
    }

    /// Keeps a prompt to bring back later, unless it repeats the last one.
    pub fn remember(&mut self, prompt: &str) {
        let prompt = prompt.trim();
        if !prompt.is_empty() && self.history.last().map(String::as_str) != Some(prompt) {
            self.history.push(prompt.to_string());
        }
    }

    /// What was typed, trimmed, to send: it is remembered and the box is left empty.
    pub fn take(&mut self) -> String {
        let text = self.text().trim().to_string();
        self.remember(&text);
        self.clear();
        self.draft.clear();
        text
    }

    /// The prompt sent before the one shown. `false`: there is none.
    pub fn older(&mut self) -> bool {
        let at = match self.recalled {
            Some(0) => return false,
            Some(at) => at - 1,
            None if self.history.is_empty() => return false,
            None => {
                self.draft = self.text();
                self.history.len() - 1
            }
        };
        let prompt = self.history[at].clone();
        self.set(&prompt);
        self.recalled = Some(at);
        true
    }

    /// The prompt sent after the one shown, and at the end what was being typed.
    pub fn newer(&mut self) -> bool {
        let Some(at) = self.recalled else {
            return false;
        };
        if at + 1 < self.history.len() {
            let prompt = self.history[at + 1].clone();
            self.set(&prompt);
            self.recalled = Some(at + 1);
        } else {
            let draft = std::mem::take(&mut self.draft);
            self.set(&draft);
            self.recalled = None;
        }
        true
    }

    /// The text cut into rows of `width` columns at most, and the cursor among them. A
    /// line longer than the width goes on in the next row.
    pub fn rows(&self, width: usize) -> Rows {
        let width = width.max(1);
        let mut rows = vec![String::new()];
        let mut used = 0;
        let mut cursor = (0, 0);
        for (at, c) in self.text.iter().enumerate() {
            if at == self.cursor {
                cursor = (rows.len() - 1, used);
            }
            if *c == '\n' {
                rows.push(String::new());
                used = 0;
                continue;
            }
            if used == width {
                rows.push(String::new());
                used = 0;
                if at == self.cursor {
                    cursor = (rows.len() - 1, 0);
                }
            }
            rows.last_mut().expect("a row").push(*c);
            used += 1;
        }
        if self.cursor == self.text.len() {
            cursor = if used == width {
                rows.push(String::new());
                (rows.len() - 1, 0)
            } else {
                (rows.len() - 1, used)
            };
        }
        Rows { rows, cursor }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> Composer {
        let mut composer = Composer::default();
        composer.insert_str(text);
        composer
    }

    #[test]
    fn typing_moving_and_deleting_edit_at_the_cursor() {
        let mut c = typed("fix the bug");
        c.word_left();
        c.insert_str("big ");
        assert_eq!(c.text(), "fix the big bug");
        c.home();
        c.delete();
        c.insert('F');
        assert_eq!(c.text(), "Fix the big bug");
        c.end();
        c.backspace();
        c.delete_word();
        assert_eq!(c.text(), "Fix the big ");
        c.delete_to_home();
        assert!(c.is_empty());
        c.backspace();
        c.left();
        c.delete();
        assert!(c.is_empty());
    }

    #[test]
    fn a_paste_keeps_its_lines_and_drops_what_moves_the_cursor() {
        let c = typed("one\r\n\ttwo\x1b[2J\rthree");
        assert_eq!(c.text(), "one\n    two[2J\nthree");
        assert!(!c.is_blank());
        assert!(typed(" \n ").is_blank());
    }

    #[test]
    fn up_and_down_move_between_lines_and_say_when_there_is_none() {
        let mut c = typed("first line\nmid\nthe last one");
        assert!(!c.down());
        assert!(c.up());
        // The column is kept as far as the line goes.
        c.insert('!');
        assert_eq!(c.text(), "first line\nmid!\nthe last one");
        assert!(c.up());
        c.insert('?');
        assert_eq!(c.text(), "firs?t line\nmid!\nthe last one");
        assert!(!c.up());
        assert!(c.down());
        assert!(c.down());
        assert!(!c.down());
    }

    #[test]
    fn sent_prompts_come_back_and_the_draft_is_kept() {
        let mut c = typed("  first  ");
        assert_eq!(c.take(), "first");
        assert!(c.is_empty());
        c.insert_str("second");
        assert_eq!(c.take(), "second");
        c.remember("second");
        c.insert_str("half typ");
        assert!(c.older());
        assert_eq!(c.text(), "second");
        assert!(c.older());
        assert_eq!(c.text(), "first");
        assert!(!c.older());
        assert!(c.newer());
        assert!(c.newer());
        assert_eq!(c.text(), "half typ");
        assert!(!c.newer());
        // Nothing sent: nothing to bring back, and nothing blank is remembered.
        let mut empty = Composer::default();
        assert_eq!(empty.take(), "");
        assert!(!empty.older());
    }

    #[test]
    fn rows_wrap_at_the_width_and_place_the_cursor() {
        let c = typed("abcdef\ngh");
        let rows = c.rows(4);
        assert_eq!(rows.rows, vec!["abcd", "ef", "gh"]);
        assert_eq!(rows.cursor, (2, 2));
        let mut c = typed("abcd");
        // At the end of a full row the cursor waits on the next one.
        assert_eq!(c.rows(4).cursor, (1, 0));
        assert_eq!(c.rows(4).rows, vec!["abcd", ""]);
        c.left();
        assert_eq!(c.rows(4).cursor, (0, 3));
        c.home();
        assert_eq!(c.rows(4).cursor, (0, 0));
        assert_eq!(Composer::default().rows(10).rows, vec![""]);
        assert_eq!(Composer::default().rows(10).cursor, (0, 0));
        let c = typed("ab\n");
        assert_eq!(c.rows(4).cursor, (1, 0));
    }
}
