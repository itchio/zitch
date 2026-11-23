//! The on-screen keyboard: four rows of ten keys in two layers over a row
//! of wider ones, for a finger or the pad.

use crate::actions::Direction;

pub const COLS: usize = 10;
/// The key rows, then the wide row.
pub const ROWS: usize = 5;
const LETTERS: [&str; 4] = ["1234567890", "qwertyuiop", "asdfghjkl'", "zxcvbnm-.!"];
const SYMBOLS: [&str; 4] = ["!@#$%^&*()", "-_=+[]{}<>", ";:'\"/\\|~`?", ",.!?&-+:'\""];
/// The wide keys with their first column and how many they span.
const WIDE: [(Key, usize, usize); 4] = [
    (Key::Symbols, 0, 2),
    (Key::Space, 2, 5),
    (Key::Delete, 7, 1),
    (Key::Done, 8, 2),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    /// Switches between the letters and the symbols.
    Symbols,
    Space,
    Delete,
    Done,
}

/// The keyboard while it is up: the key with pad focus and the layer.
#[derive(Debug)]
pub struct Keyboard {
    row: usize,
    col: usize,
    pub symbols: bool,
    /// The key last pressed and when, for its flash.
    pub pressed: Option<(usize, usize, std::time::Instant)>,
}

impl Default for Keyboard {
    /// Focus starts on G, in the middle of the letters.
    fn default() -> Self {
        Self {
            row: 2,
            col: 4,
            symbols: false,
            pressed: None,
        }
    }
}

impl Keyboard {
    /// The key at a place in the grid, with the columns it covers.
    pub fn key_at(&self, row: usize, col: usize) -> Option<(Key, std::ops::Range<usize>)> {
        if row == ROWS - 1 {
            return WIDE
                .iter()
                .find(|(_, first, span)| (*first..first + span).contains(&col))
                .map(|&(key, first, span)| (key, first..first + span));
        }
        let layer = if self.symbols { SYMBOLS } else { LETTERS };
        let c = layer.get(row)?.chars().nth(col)?;
        Some((Key::Char(c), col..col + 1))
    }

    /// Every key, each once, with its row and columns.
    pub fn keys(&self) -> impl Iterator<Item = (Key, usize, std::ops::Range<usize>)> + '_ {
        (0..ROWS).flat_map(move |row| {
            (0..COLS).filter_map(move |col| {
                let (key, cols) = self.key_at(row, col)?;
                (cols.start == col).then_some((key, row, cols))
            })
        })
    }

    pub fn focused(&self) -> Option<Key> {
        self.key_at(self.row, self.col).map(|(key, _)| key)
    }

    pub fn has_focus(&self, row: usize, cols: &std::ops::Range<usize>) -> bool {
        self.row == row && cols.contains(&self.col)
    }

    pub fn focus(&mut self, row: usize, col: usize) {
        self.row = row.min(ROWS - 1);
        self.col = col.min(COLS - 1);
    }

    /// Marks the focused key as just pressed.
    pub fn press(&mut self) {
        self.pressed = Some((self.row, self.col, std::time::Instant::now()));
    }

    /// How bright a key's flash still is, 1 at the press down to 0.
    pub fn flash(&self, row: usize, cols: &std::ops::Range<usize>) -> f32 {
        const FLASH: f32 = 0.18;
        match self.pressed {
            Some((r, c, at)) if r == row && cols.contains(&c) => {
                (1.0 - at.elapsed().as_secs_f32() / FLASH).max(0.0)
            }
            _ => 0.0,
        }
    }

    /// Moves focus one key over, wrapping at the edges. Leaving the wide
    /// row keeps the column the focus came in on.
    pub fn step(&mut self, direction: Direction) {
        match direction {
            Direction::Up => self.row = (self.row + ROWS - 1) % ROWS,
            Direction::Down => self.row = (self.row + 1) % ROWS,
            Direction::Left | Direction::Right => {
                let Some((_, cols)) = self.key_at(self.row, self.col) else {
                    return;
                };
                self.col = if direction == Direction::Left {
                    (cols.start + COLS - 1) % COLS
                } else {
                    cols.end % COLS
                };
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_place_in_the_grid_has_a_key() {
        for symbols in [false, true] {
            let keyboard = Keyboard {
                symbols,
                ..Default::default()
            };
            for row in 0..ROWS {
                for col in 0..COLS {
                    assert!(keyboard.key_at(row, col).is_some(), "{row},{col}");
                }
            }
            assert_eq!(keyboard.keys().count(), 44);
        }
    }

    #[test]
    fn focus_steps_over_a_wide_key_whole() {
        let mut keyboard = Keyboard::default();
        keyboard.focus(4, 4);
        assert_eq!(keyboard.focused(), Some(Key::Space));
        keyboard.step(Direction::Right);
        assert_eq!(keyboard.focused(), Some(Key::Delete));
        keyboard.step(Direction::Left);
        keyboard.step(Direction::Left);
        assert_eq!(keyboard.focused(), Some(Key::Symbols));
        keyboard.step(Direction::Left);
        assert_eq!(keyboard.focused(), Some(Key::Done));
    }

    #[test]
    fn focus_wraps_from_the_top_row_to_the_wide_one() {
        let mut keyboard = Keyboard::default();
        keyboard.focus(0, 9);
        keyboard.step(Direction::Up);
        assert_eq!(keyboard.focused(), Some(Key::Done));
        keyboard.step(Direction::Down);
        assert_eq!(keyboard.focused(), Some(Key::Char('0')));
    }
}
