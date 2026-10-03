use super::*;
use std::char;
use std::io::{stdout, Read, Write};
use termion::async_stdin;
use termion::event::Key;
use termion::input::TermRead;
use termion::raw::IntoRawMode;
use termion::screen::AlternateScreen;
use termion::{color, cursor};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

// Matches tmux's default `tab-stop` setting, used by capture-pane output
// (tabs are preserved verbatim rather than expanded to spaces).
// Keep in sync with the TAB_STOP in swapper.rs (duplicated there since each
// [[bin]] in this crate is its own crate root).
const TAB_STOP: usize = 8;

// unicode-width reports 0/None width for '\t' since a tab's rendered width
// depends on the current column, not the character itself. Expand it to the
// next tab stop so prefix widths (and therefore hint offsets) stay accurate
// on lines that contain literal tabs, e.g. `git status` file listings.
//
// Uses width() rather than width_cjk(): width_cjk() treats Unicode
// "Ambiguous"-category characters (which includes the Private-Use-Area glyphs
// used by Nerd Font / Powerline prompt themes) as double-width, but real
// terminals (and tmux) render them single-width unless in a CJK locale.
// width() matches that common-case rendering.
fn display_width(text: &str) -> usize {
  let mut col = 0;

  for ch in text.chars() {
    if ch == '\t' {
      col += TAB_STOP - (col % TAB_STOP);
    } else {
      col += ch.width().unwrap_or(0);
    }
  }

  col
}

pub struct View<'a> {
  state: &'a mut state::State<'a>,
  skip: usize,
  multi: bool,
  contrast: bool,
  mask: bool,
  position: &'a str,
  matches: Vec<state::Match<'a>>,
  select_foreground_color: Box<dyn color::Color>,
  select_background_color: Box<dyn color::Color>,
  multi_foreground_color: Box<dyn color::Color>,
  multi_background_color: Box<dyn color::Color>,
  foreground_color: Box<dyn color::Color>,
  background_color: Box<dyn color::Color>,
  hint_background_color: Box<dyn color::Color>,
  hint_foreground_color: Box<dyn color::Color>,
  chosen: Vec<(String, bool, String)>,
}

enum CaptureEvent {
  Exit,
  Hint,
}

impl<'a> View<'a> {
  pub fn new(
    state: &'a mut state::State<'a>,
    multi: bool,
    reverse: bool,
    unique: bool,
    contrast: bool,
    mask: bool,
    position: &'a str,
    select_foreground_color: Box<dyn color::Color>,
    select_background_color: Box<dyn color::Color>,
    multi_foreground_color: Box<dyn color::Color>,
    multi_background_color: Box<dyn color::Color>,
    foreground_color: Box<dyn color::Color>,
    background_color: Box<dyn color::Color>,
    hint_foreground_color: Box<dyn color::Color>,
    hint_background_color: Box<dyn color::Color>,
  ) -> View<'a> {
    let matches = state.matches(reverse, unique);
    let skip = if reverse { matches.len() - 1 } else { 0 };

    View {
      state,
      skip,
      multi,
      contrast,
      mask,
      position,
      matches,
      select_foreground_color,
      select_background_color,
      multi_foreground_color,
      multi_background_color,
      foreground_color,
      background_color,
      hint_foreground_color,
      hint_background_color,
      chosen: vec![],
    }
  }

  pub fn prev(&mut self) {
    if self.skip > 0 {
      self.skip -= 1;
    }
  }

  pub fn next(&mut self) {
    if self.skip < self.matches.len() - 1 {
      self.skip += 1;
    }
  }

  fn make_hint_text(&self, hint: &str) -> String {
    if self.contrast {
      format!("[{}]", hint)
    } else {
      hint.to_string()
    }
  }

  fn render(&self, stdout: &mut dyn Write, typed_hint: &str) -> () {
    write!(stdout, "{}", cursor::Hide).unwrap();

    // With `mask`, everything that isn't a match stays blank: only the matched
    // zones (redrawn below at their own position) are visible.
    if !self.mask {
      for (index, line) in self.state.lines.iter().enumerate() {
        let clean = line.trim_end_matches(|c: char| c.is_whitespace());

        if !clean.is_empty() {
          print!("{goto}{text}", goto = cursor::Goto(1, index as u16 + 1), text = line);
        }
      }
    }

    let selected = self.matches.get(self.skip);

    for mat in self.matches.iter() {
      let chosen_hint = self.chosen.iter().any(|(hint, _, _)| hint == mat.text);

      let selected_color = if chosen_hint {
        &self.multi_foreground_color
      } else if selected == Some(mat) {
        &self.select_foreground_color
      } else {
        &self.foreground_color
      };
      let selected_background_color = if chosen_hint {
        &self.multi_background_color
      } else if selected == Some(mat) {
        &self.select_background_color
      } else {
        &self.background_color
      };

      // mat.x is a byte offset (from the regex match); convert it to a
      // display-column offset so wide (CJK/emoji) and narrow multi-byte
      // (accented letters, powerline glyphs, etc.) characters are handled
      // consistently.
      let line = &self.state.lines[mat.y as usize];
      let prefix = &line[0..mat.x as usize];
      let offset = display_width(prefix) as u16;
      let text = self.make_hint_text(mat.text);

      print!(
        "{goto}{background}{foregroud}{text}{resetf}{resetb}",
        goto = cursor::Goto(offset + 1, mat.y as u16 + 1),
        foregroud = color::Fg(&**selected_color),
        background = color::Bg(&**selected_background_color),
        resetf = color::Fg(color::Reset),
        resetb = color::Bg(color::Reset),
        text = &text
      );

      if let Some(ref hint) = mat.hint {
        let extra_position = match self.position {
          "right" => text.width() - hint.len(),
          "off_left" => 0 - hint.len() - if self.contrast { 2 } else { 0 },
          "off_right" => text.width(),
          _ => 0,
        };

        let text = self.make_hint_text(hint.as_str());
        let final_position = std::cmp::max(offset as i16 + extra_position as i16, 0);

        print!(
          "{goto}{background}{foregroud}{text}{resetf}{resetb}",
          goto = cursor::Goto(final_position as u16 + 1, mat.y as u16 + 1),
          foregroud = color::Fg(&*self.hint_foreground_color),
          background = color::Bg(&*self.hint_background_color),
          resetf = color::Fg(color::Reset),
          resetb = color::Bg(color::Reset),
          text = &text
        );

        if hint.starts_with(typed_hint) {
          print!(
            "{goto}{background}{foregroud}{text}{resetf}{resetb}",
            goto = cursor::Goto(final_position as u16 + 1, mat.y as u16 + 1),
            foregroud = color::Fg(&*self.multi_foreground_color),
            background = color::Bg(&*self.multi_background_color),
            resetf = color::Fg(color::Reset),
            resetb = color::Bg(color::Reset),
            text = &typed_hint
          );
        }
      }
    }

    stdout.flush().unwrap();
  }

  fn listen(&mut self, stdin: &mut dyn Read, stdout: &mut dyn Write) -> CaptureEvent {
    if self.matches.is_empty() {
      return CaptureEvent::Exit;
    }

    let mut typed_hint: String = "".to_owned();
    let longest_hint = self
      .matches
      .iter()
      .filter_map(|m| m.hint.clone())
      .max_by(|x, y| x.len().cmp(&y.len()))
      .unwrap()
      .clone();

    self.render(stdout, &typed_hint);

    loop {
      match stdin.keys().next() {
        Some(key) => {
          match key {
            Ok(key) => {
              match key {
                Key::Esc => {
                  if self.multi && !typed_hint.is_empty() {
                    typed_hint.clear();
                  } else {
                    break;
                  }
                }
                Key::Up => {
                  self.prev();
                }
                Key::Down => {
                  self.next();
                }
                Key::Left => {
                  self.prev();
                }
                Key::Right => {
                  self.next();
                }
                Key::Backspace => {
                  typed_hint.pop();
                }
                Key::Char(ch) => {
                  match ch {
                    '\n' => match self.matches.iter().enumerate().find(|&h| h.0 == self.skip) {
                      Some(hm) => {
                        self
                          .chosen
                          .push((hm.1.text.to_string(), false, hm.1.pattern.to_string()));

                        if !self.multi {
                          return CaptureEvent::Hint;
                        }
                      }
                      _ => panic!("Match not found?"),
                    },
                    ' ' => {
                      if self.multi {
                        // Finalize the multi selection
                        return CaptureEvent::Hint;
                      } else {
                        // Enable the multi selection
                        self.multi = true;
                      }
                    }
                    key => {
                      let key = key.to_string();
                      let lower_key = key.to_lowercase();

                      typed_hint.push_str(lower_key.as_str());

                      let selection = self.matches.iter().find(|mat| mat.hint == Some(typed_hint.clone()));

                      match selection {
                        Some(mat) => {
                          self
                            .chosen
                            .push((mat.text.to_string(), key != lower_key, mat.pattern.to_string()));

                          if self.multi {
                            typed_hint.clear();
                          } else {
                            return CaptureEvent::Hint;
                          }
                        }
                        None => {
                          if !self.multi && typed_hint.len() >= longest_hint.len() {
                            break;
                          }
                        }
                      }
                    }
                  }
                }
                _ => {
                  // Unknown key
                }
              }
            }
            Err(err) => panic!("{}", err),
          }

          stdin.keys().for_each(|_| { /* Skip the rest of stdin buffer */ })
        }
        _ => {
          // Nothing in the buffer. Wait for a bit...
          std::thread::sleep(std::time::Duration::from_millis(50));
          continue; // don't render again if nothing new to show
        }
      }

      self.render(stdout, &typed_hint);
    }

    CaptureEvent::Exit
  }

  pub fn present(&mut self) -> Vec<(String, bool, String)> {
    let mut stdin = async_stdin();
    let mut stdout = AlternateScreen::from(stdout().into_raw_mode().unwrap());

    let hints = match self.listen(&mut stdin, &mut stdout) {
      CaptureEvent::Exit => vec![],
      CaptureEvent::Hint => self.chosen.clone(),
    };

    write!(stdout, "{}", cursor::Show).unwrap();

    hints
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn split(output: &str) -> Vec<&str> {
    output.split("\n").collect::<Vec<&str>>()
  }

  #[test]
  fn hint_text() {
    let lines = split("lorem 127.0.0.1 lorem");
    let custom = [].to_vec();
    let mut state = state::State::new(&lines, "abcd", &custom, None);
    let mut view = View {
      state: &mut state,
      skip: 0,
      multi: false,
      contrast: false,
      mask: false,
      position: &"",
      matches: vec![],
      select_foreground_color: colors::get_color("default"),
      select_background_color: colors::get_color("default"),
      multi_foreground_color: colors::get_color("default"),
      multi_background_color: colors::get_color("default"),
      foreground_color: colors::get_color("default"),
      background_color: colors::get_color("default"),
      hint_background_color: colors::get_color("default"),
      hint_foreground_color: colors::get_color("default"),
      chosen: vec![],
    };

    let result = view.make_hint_text("a");
    assert_eq!(result, "a".to_string());

    view.contrast = true;
    let result = view.make_hint_text("a");
    assert_eq!(result, "[a]".to_string());
  }

  #[test]
  fn tab_prefix_offset() {
    let lines = split("\tscripts/xmpp");
    let custom = [].to_vec();
    let state = state::State::new(&lines, "abcd", &custom, None);
    let matches = state.matches(false, false);
    let mat = matches
      .iter()
      .find(|m| m.text == "scripts/xmpp")
      .expect("match not found");
    let line = &lines[mat.y as usize];
    let prefix = &line[0..mat.x as usize];

    // A tab at column 0 must expand to the next tab stop (column 8), not be
    // treated as zero-width, so the hint lands on "scripts/xmpp" instead of
    // over the leading whitespace.
    assert_eq!(display_width(prefix), 8);
  }

  #[test]
  fn display_width_treats_powerline_glyphs_as_narrow() {
    // U+E0B0 is a Private-Use-Area glyph (Nerd Font / Powerline prompt
    // separator). Real terminals render it single-width; width_cjk() would
    // wrongly count it double-width (Ambiguous category), shifting every
    // hint one column to the right on themed prompt lines.
    assert_eq!(display_width("\u{e0b0} "), 2);
  }
}
