extern crate clap;

use self::clap::{App, Arg};
use clap::crate_version;
use regex::Regex;
use std::io::Write;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_width::UnicodeWidthChar;

trait Executor {
  fn execute(&mut self, args: Vec<String>) -> String;
  fn last_executed(&self) -> Option<Vec<String>>;
}

struct RealShell {
  executed: Option<Vec<String>>,
}

impl RealShell {
  fn new() -> RealShell {
    RealShell { executed: None }
  }
}

impl Executor for RealShell {
  fn execute(&mut self, args: Vec<String>) -> String {
    let execution = Command::new(args[0].as_str())
      .args(&args[1..])
      .output()
      .expect("Couldn't run it");

    self.executed = Some(args);

    let output: String = String::from_utf8_lossy(&execution.stdout).into();

    output.trim_end().to_string()
  }

  fn last_executed(&self) -> Option<Vec<String>> {
    self.executed.clone()
  }
}

const TMP_FILE: &str = "/tmp/thumbs-last";

// Keep in sync with the TAB_STOP in view.rs — duplicated here because each
// [[bin]] in this crate is its own crate root, so view.rs's mod isn't visible
// from this binary.
const TAB_STOP: usize = 8;

// Inverse of view.rs's display_width: given a target display column, find how
// many bytes of `line` are needed to reach it, so a `cursor_x` column (as
// reported by tmux, which counts wide/tab-expanded display cells, not bytes)
// can be turned into a valid `&str` slice boundary.
//
// Uses width() rather than width_cjk(): width_cjk() treats Unicode
// "Ambiguous"-category characters (which includes the Private-Use-Area glyphs
// used by Nerd Font / Powerline prompt themes) as double-width, but real
// terminals (and tmux) render them single-width unless in a CJK locale.
// width() matches that common-case rendering.
fn byte_offset_for_column(line: &str, target_col: usize) -> usize {
  let mut col = 0;
  let mut offset = 0;

  for ch in line.chars() {
    if col >= target_col {
      break;
    }

    col += if ch == '\t' {
      TAB_STOP - (col % TAB_STOP)
    } else {
      ch.width().unwrap_or(0)
    };

    offset += ch.len_utf8();
  }

  offset
}

// A "word" here is any run of non-blank characters, not just \w (alnum/underscore)
// — so it covers paths, URLs, flags, etc. It's whatever contiguous, blank-delimited
// token the user was in the middle of typing, matching the cursor_word_regexp built
// in execute_thumbs (below), which looks for that whole token as a \S*-bounded match.
fn extract_word_before_cursor(line: &str, cursor_col: usize) -> Option<String> {
  let offset = byte_offset_for_column(line, cursor_col);
  let prefix = &line[..offset];

  Regex::new(r"(\S+)$")
    .unwrap()
    .captures(prefix)
    .map(|c| c[1].to_string())
}

#[allow(dead_code)]
fn dbg(msg: &str) {
  let mut file = std::fs::OpenOptions::new()
    .create(true)
    .write(true)
    .append(true)
    .open("/tmp/thumbs.log")
    .expect("Unable to open log file");

  writeln!(&mut file, "{}", msg).expect("Unable to write log file");
}

pub struct Swapper<'a> {
  executor: Box<&'a mut dyn Executor>,
  dir: String,
  command: String,
  upcase_command: String,
  multi_command: String,
  osc52: bool,
  active_pane_id: Option<String>,
  active_pane_height: Option<i32>,
  active_pane_scroll_position: Option<i32>,
  active_pane_zoomed: Option<bool>,
  active_pane_in_mode: Option<bool>,
  active_pane_cursor_x: Option<i32>,
  active_pane_cursor_y: Option<i32>,
  cursor_word: Option<String>,
  thumbs_pane_id: Option<String>,
  content: Option<String>,
  signal: String,
}

impl<'a> Swapper<'a> {
  fn new(
    executor: Box<&'a mut dyn Executor>,
    dir: String,
    command: String,
    upcase_command: String,
    multi_command: String,
    osc52: bool,
  ) -> Swapper {
    let since_the_epoch = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .expect("Time went backwards");
    let signal = format!("thumbs-finished-{}", since_the_epoch.as_secs());

    Swapper {
      executor,
      dir,
      command,
      upcase_command,
      multi_command,
      osc52,
      active_pane_id: None,
      active_pane_height: None,
      active_pane_scroll_position: None,
      active_pane_zoomed: None,
      active_pane_in_mode: None,
      active_pane_cursor_x: None,
      active_pane_cursor_y: None,
      cursor_word: None,
      thumbs_pane_id: None,
      content: None,
      signal,
    }
  }

  pub fn capture_active_pane(&mut self) {
    let active_command = vec![
      "tmux",
      "list-panes",
      "-F",
      "#{pane_id}:#{?pane_in_mode,1,0}:#{pane_height}:#{scroll_position}:#{window_zoomed_flag}:#{?pane_active,active,nope}:#{cursor_x}:#{cursor_y}",
    ];

    let output = self
      .executor
      .execute(active_command.iter().map(|arg| arg.to_string()).collect());

    let lines: Vec<&str> = output.split('\n').collect();
    let chunks: Vec<Vec<&str>> = lines.into_iter().map(|line| line.split(':').collect()).collect();

    let active_pane = chunks
      .iter()
      .find(|&chunks| *chunks.get(5).unwrap() == "active")
      .expect("Unable to find active pane");

    let pane_id = active_pane.get(0).unwrap();

    self.active_pane_id = Some(pane_id.to_string());

    let pane_height = active_pane
      .get(2)
      .unwrap()
      .parse()
      .expect("Unable to retrieve pane height");

    self.active_pane_height = Some(pane_height);

    let in_copy_mode = active_pane.get(1).unwrap().to_string() == "1";

    self.active_pane_in_mode = Some(in_copy_mode);

    if in_copy_mode {
      let pane_scroll_position = active_pane
        .get(3)
        .unwrap()
        .parse()
        .expect("Unable to retrieve pane scroll");

      self.active_pane_scroll_position = Some(pane_scroll_position);
    }

    let zoomed_pane = *active_pane.get(4).expect("Unable to retrieve zoom pane property") == "1";

    self.active_pane_zoomed = Some(zoomed_pane);

    self.active_pane_cursor_x = active_pane.get(6).and_then(|v| v.parse::<i32>().ok());
    self.active_pane_cursor_y = active_pane.get(7).and_then(|v| v.parse::<i32>().ok());
  }

  // Only supports the live cursor in normal mode. If the pane is in copy-mode
  // (scrolled back), skip silently rather than trying to resolve
  // copy_cursor_x/copy_cursor_y. This eager capture-pane call races against
  // the pipeline's own later capture (see pane_command in execute_thumbs) —
  // worst case the derived word is based on slightly stale pane text, which
  // is an accepted limitation for a "just parked the cursor" feature.
  fn capture_cursor_word(&mut self) -> Option<String> {
    if self.active_pane_in_mode != Some(false) {
      return None;
    }

    let cursor_x = self.active_pane_cursor_x?;
    let cursor_y = self.active_pane_cursor_y?;
    let active_pane_id = self.active_pane_id.clone()?;

    let capture_command = vec!["tmux", "capture-pane", "-p", "-t", active_pane_id.as_str()];
    let params: Vec<String> = capture_command.iter().map(|arg| arg.to_string()).collect();
    let output = self.executor.execute(params);

    let rows: Vec<&str> = output.split('\n').collect();
    let row = rows.get(cursor_y.max(0) as usize)?;

    extract_word_before_cursor(row, cursor_x.max(0) as usize)
  }

  pub fn execute_thumbs(&mut self) {
    let options_command = vec!["tmux", "show", "-g"];
    let params: Vec<String> = options_command.iter().map(|arg| arg.to_string()).collect();
    let options = self.executor.execute(params);
    let lines: Vec<&str> = options.split('\n').collect();

    let pattern = Regex::new(r#"^@thumbs-([\w\-0-9]+)\s+"?([^"]+)"?$"#).unwrap();

    let mut args = lines
      .iter()
      .flat_map(|line| {
        if let Some(captures) = pattern.captures(line) {
          let name = captures.get(1).unwrap().as_str();
          let value = captures.get(2).unwrap().as_str();

          let boolean_params = vec!["reverse", "unique", "contrast", "mask"];

          if boolean_params.iter().any(|&x| x == name) {
            return vec![format!("--{}", name)];
          }

          let string_params = vec![
            "alphabet",
            "position",
            "fg-color",
            "bg-color",
            "hint-bg-color",
            "hint-fg-color",
            "select-fg-color",
            "select-bg-color",
            "multi-fg-color",
            "multi-bg-color",
          ];

          if string_params.iter().any(|&x| x == name) {
            return vec![format!("--{}", name), format!("'{}'", value)];
          }

          if name.starts_with("regexp") {
            return vec!["--regexp".to_string(), format!("'{}'", value.replace("\\\\", "\\"))];
          }

          vec![]
        } else {
          vec![]
        }
      })
      .collect::<Vec<String>>();

    // Not part of `boolean_params` above: that list is forwarded verbatim as
    // CLI flags to `thumbs`, which has no `--cursor-word` flag and would
    // error out on an unrecognized argument. This is swapper-only config.
    let cursor_word_enabled = lines.iter().any(|line| {
      pattern
        .captures(line)
        .map(|captures| captures.get(1).unwrap().as_str() == "cursor-word")
        .unwrap_or(false)
    });

    let active_pane_id = self.active_pane_id.as_mut().unwrap().clone();

    if cursor_word_enabled {
      if let Some(word) = self.capture_cursor_word() {
        self.cursor_word = Some(word.clone());
        args.push("--cursor-word-regexp".to_string());

        // `word` can now contain arbitrary punctuation (see extract_word_before_cursor),
        // so it must be regex-escaped before being spliced into a pattern, and the result
        // is still embedded in a single-quoted shell string below, so any literal `'` left
        // over also needs escaping for that context.
        let regex_safe = regex::escape(&word);
        let shell_safe = regex_safe.replace('\'', r"'\''");

        args.push(format!("'\\S*{}\\S*'", shell_safe));
      }
    }

    let scroll_params =
      if let (Some(pane_height), Some(scroll_position)) = (self.active_pane_height, self.active_pane_scroll_position) {
        format!(" -S {} -E {}", -scroll_position, pane_height - scroll_position - 1)
      } else {
        "".to_string()
      };

    let active_pane_zoomed = self.active_pane_zoomed.as_mut().unwrap().clone();
    let zoom_command = if active_pane_zoomed {
      format!("tmux resize-pane -t {} -Z;", active_pane_id)
    } else {
      "".to_string()
    };

    let pane_command = format!(
        "tmux capture-pane -J -t {active_pane_id} -p{scroll_params} | tail -n {height} | {dir}/target/release/thumbs -f '%U:%P:%H' -t {tmp} {args}; tmux swap-pane -t {active_pane_id}; {zoom_command} tmux wait-for -S {signal}",
        active_pane_id = active_pane_id,
        scroll_params = scroll_params,
        height = self.active_pane_height.unwrap_or(i32::MAX),
        dir = self.dir,
        tmp = TMP_FILE,
        args = args.join(" "),
        zoom_command = zoom_command,
        signal = self.signal
    );

    let thumbs_command = vec![
      "tmux",
      "new-window",
      "-P",
      "-F",
      "#{pane_id}",
      "-d",
      "-n",
      "[thumbs]",
      pane_command.as_str(),
    ];

    let params: Vec<String> = thumbs_command.iter().map(|arg| arg.to_string()).collect();

    self.thumbs_pane_id = Some(self.executor.execute(params));
  }

  pub fn swap_panes(&mut self) {
    let active_pane_id = self.active_pane_id.as_mut().unwrap().clone();
    let thumbs_pane_id = self.thumbs_pane_id.as_mut().unwrap().clone();

    let swap_command = vec![
      "tmux",
      "swap-pane",
      "-d",
      "-s",
      active_pane_id.as_str(),
      "-t",
      thumbs_pane_id.as_str(),
    ];

    let params = swap_command
      .iter()
      .filter(|&s| !s.is_empty())
      .map(|arg| arg.to_string())
      .collect();

    self.executor.execute(params);
  }

  pub fn resize_pane(&mut self) {
    let active_pane_zoomed = self.active_pane_zoomed.as_mut().unwrap().clone();

    if !active_pane_zoomed {
      return;
    }

    let thumbs_pane_id = self.thumbs_pane_id.as_mut().unwrap().clone();

    let resize_command = vec!["tmux", "resize-pane", "-t", thumbs_pane_id.as_str(), "-Z"];

    let params = resize_command
      .iter()
      .filter(|&s| !s.is_empty())
      .map(|arg| arg.to_string())
      .collect();

    self.executor.execute(params);
  }

  pub fn wait_thumbs(&mut self) {
    let wait_command = vec!["tmux", "wait-for", self.signal.as_str()];
    let params = wait_command.iter().map(|arg| arg.to_string()).collect();

    self.executor.execute(params);
  }

  pub fn retrieve_content(&mut self) {
    let retrieve_command = vec!["cat", TMP_FILE];
    let params = retrieve_command.iter().map(|arg| arg.to_string()).collect();

    self.content = Some(self.executor.execute(params));
  }

  pub fn destroy_content(&mut self) {
    let retrieve_command = vec!["rm", TMP_FILE];
    let params = retrieve_command.iter().map(|arg| arg.to_string()).collect();

    self.executor.execute(params);
  }

  pub fn send_osc52(&mut self) {}

  pub fn execute_command(&mut self) {
    let content = self.content.clone().unwrap();
    let items: Vec<&str> = content.split('\n').collect();

    if items.len() > 1 {
      let text = items
        .iter()
        .map(|item| item.splitn(3, ':').last().unwrap())
        .collect::<Vec<&str>>()
        .join(" ");

      self.execute_final_command(&text, &self.multi_command.clone());

      return;
    }

    // Only one item
    let item: &str = items.first().unwrap();

    let mut splitter = item.splitn(3, ':');

    if let Some(upcase) = splitter.next() {
      if let Some(pattern) = splitter.next() {
        if let Some(text) = splitter.next() {
          if upcase.trim_end() == "true" && pattern.trim_end() == "cursor_word" {
            self.delete_cursor_word();
          }

          if self.osc52 {
            let base64_text = base64::encode(text.as_bytes());
            let osc_seq = format!("\x1b]52;0;{}\x07", base64_text);
            let tmux_seq = format!("\x1bPtmux;{}\x1b\\", osc_seq.replace("\x1b", "\x1b\x1b"));

            // FIXME: Review if this comment is still rellevant
            //
            // When the user selects a match:
            // 1. The `rustbox` object created in the `viewbox` above is dropped.
            // 2. During its `drop`, the `rustbox` object sends a CSI 1049 escape
            //    sequence to tmux.
            // 3. This escape sequence causes the `window_pane_alternate_off` function
            //    in tmux to be called.
            // 4. In `window_pane_alternate_off`, tmux sets the needs-redraw flag in the
            //    pane.
            // 5. If we print the OSC copy escape sequence before the redraw is completed,
            //    tmux will *not* send the sequence to the host terminal. See the following
            //    call chain in tmux: `input_dcs_dispatch` -> `screen_write_rawstring`
            //    -> `tty_write` -> `tty_client_ready`. In this case, `tty_client_ready`
            //    will return false, thus preventing the escape sequence from being sent.
            //
            // Therefore, for now we wait a little bit here for the redraw to finish.
            std::thread::sleep(std::time::Duration::from_millis(100));

            std::io::stdout().write_all(tmux_seq.as_bytes()).unwrap();
            std::io::stdout().flush().unwrap();
          }

          let execute_command = if upcase.trim_end() == "true" {
            self.upcase_command.clone()
          } else {
            self.command.clone()
          };

          // The command we run has two arguments:
          //  * The first arg is the (trimmed) text. This gets stored in a variable, in order to
          //    preserve quoting and special characters.
          //
          //  * The second argument is the user's command, with the '{}' token replaced with an
          //    unquoted reference to the variable containing the text.
          //
          // The reference is unquoted, unfortunately, because the token may already have been
          // spliced into a string (e.g 'tmux display-message "Copied {}"'), and it's impossible (or
          // at least exceedingly difficult) to determine the correct quoting level.
          //
          // The alternative of literally splicing the text into the command is bad and it causes all
          // kinds of harmful escaping issues that the user cannot reasonable avoid.
          //
          // For example, imagine some pattern matched the text "foo;rm *" and the user's command was
          // an innocuous "echo {}". With literal splicing, we would run the command "echo foo;rm *".
          // That's BAD. Without splicing, instead we execute "echo ${THUMB}" which does mostly the
          // right thing regardless the contents of the text. (At worst, bash will word-separate the
          // unquoted variable; but it won't _execute_ those words in common scenarios).
          //
          // Ideally user commands would just use "${THUMB}" to begin with rather than having any
          // sort of ad-hoc string splicing here at all, and then they could specify the quoting they
          // want, but that would break backwards compatibility.
          self.execute_final_command(text.trim_end(), &execute_command);
        }
      }
    }
  }

  fn delete_cursor_word(&mut self) {
    if let (Some(word), Some(active_pane_id)) = (self.cursor_word.clone(), self.active_pane_id.clone()) {
      let mut params: Vec<String> = vec![
        "tmux".to_string(),
        "send-keys".to_string(),
        "-t".to_string(),
        active_pane_id,
      ];

      for _ in 0..word.chars().count() {
        params.push("BSpace".to_string());
      }

      self.executor.execute(params);
    }
  }

  pub fn execute_final_command(&mut self, text: &str, execute_command: &str) {
    let final_command = str::replace(execute_command, "{}", "${THUMB}");
    let retrieve_command = vec![
      "bash",
      "-c",
      "THUMB=\"$1\"; eval \"$2\"",
      "--",
      text,
      final_command.as_str(),
    ];

    let params = retrieve_command.iter().map(|arg| arg.to_string()).collect();

    self.executor.execute(params);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  struct TestShell {
    outputs: Vec<String>,
    executed: Option<Vec<String>>,
    history: Vec<Vec<String>>,
  }

  impl TestShell {
    fn new(outputs: Vec<String>) -> TestShell {
      TestShell {
        executed: None,
        outputs,
        history: vec![],
      }
    }
  }

  impl Executor for TestShell {
    fn execute(&mut self, args: Vec<String>) -> String {
      self.executed = Some(args.clone());
      self.history.push(args);
      self.outputs.pop().unwrap()
    }

    fn last_executed(&self) -> Option<Vec<String>> {
      self.executed.clone()
    }
  }

  #[test]
  fn retrieve_active_pane() {
    let last_command_outputs = vec!["%97:100:24:1:0:active\n%106:100:24:1:0:nope\n%107:100:24:1:0:nope\n".to_string()];
    let mut executor = TestShell::new(last_command_outputs);
    let mut swapper = Swapper::new(
      Box::new(&mut executor),
      "".to_string(),
      "".to_string(),
      "".to_string(),
      "".to_string(),
      false,
    );

    swapper.capture_active_pane();

    assert_eq!(swapper.active_pane_id.unwrap(), "%97");
  }

  #[test]
  fn swap_panes() {
    let last_command_outputs = vec![
      "".to_string(),
      "%100".to_string(),
      "".to_string(),
      "%106:100:24:1:0:nope\n%98:100:24:1:0:active\n%107:100:24:1:0:nope\n".to_string(),
    ];
    let mut executor = TestShell::new(last_command_outputs);
    let mut swapper = Swapper::new(
      Box::new(&mut executor),
      "".to_string(),
      "".to_string(),
      "".to_string(),
      "".to_string(),
      false,
    );

    swapper.capture_active_pane();
    swapper.execute_thumbs();
    swapper.swap_panes();

    let expectation = vec!["tmux", "swap-pane", "-d", "-s", "%98", "-t", "%100"];

    assert_eq!(executor.last_executed().unwrap(), expectation);
  }

  #[test]
  fn quoted_execution() {
    let last_command_outputs = vec!["Blah blah blah, the ignored user script output".to_string()];
    let mut executor = TestShell::new(last_command_outputs);

    let user_command = "echo \"{}\"".to_string();
    let upcase_command = "open \"{}\"".to_string();
    let multi_command = "open \"{}\"".to_string();
    let mut swapper = Swapper::new(
      Box::new(&mut executor),
      "".to_string(),
      user_command,
      upcase_command,
      multi_command,
      false,
    );

    swapper.content = Some(format!(
      "{do_upcase}:{pattern}:{thumb_text}",
      do_upcase = false,
      pattern = "path",
      thumb_text = "foobar;rm *",
    ));
    swapper.execute_command();

    let expectation = vec![
      "bash",
      // The actual shell command:
      "-c",
      "THUMB=\"$1\"; eval \"$2\"",
      // $0: The non-existent program name.
      "--",
      // $1: The value assigned to THUMB above.
      //     Not interpreted as a shell expression!
      "foobar;rm *",
      // $2: The user script, with {} replaced with ${THUMB},
      //     and will be eval'd with THUMB in scope.
      "echo \"${THUMB}\"",
    ];

    assert_eq!(executor.last_executed().unwrap(), expectation);
  }

  #[test]
  fn word_before_cursor() {
    assert_eq!(extract_word_before_cursor("hello world", 5), Some("hello".to_string()));
    assert_eq!(extract_word_before_cursor("hello world", 11), Some("world".to_string()));
    assert_eq!(extract_word_before_cursor("hello ", 6), None);
    assert_eq!(extract_word_before_cursor("", 0), None);
    assert_eq!(extract_word_before_cursor("hello", 999), Some("hello".to_string()));
  }

  #[test]
  fn word_before_cursor_is_whole_blank_delimited_token() {
    // The "word" is whatever non-blank token the cursor sits in, not just a
    // run of \w characters — so a half-typed path is captured in full rather
    // than truncated to its last path segment.
    let line = "cd /home/fdie/.asdf/installs/rust/1.91.0/bin/alacr";
    assert_eq!(
      extract_word_before_cursor(line, line.chars().count()),
      Some("/home/fdie/.asdf/installs/rust/1.91.0/bin/alacr".to_string())
    );
  }

  #[test]
  fn word_before_cursor_with_wide_chars() {
    // "日本語 " = 3 double-width chars (cols 0-5) + space (col 6) = 7 cols,
    // then "hello" occupies cols 7-11, so col 12 sits right after the final 'o'.
    assert_eq!(
      extract_word_before_cursor("日本語 hello", 12),
      Some("hello".to_string())
    );
  }

  #[test]
  fn word_before_cursor_with_powerline_glyph() {
    // U+E0B0 is a Private-Use-Area glyph (Nerd Font / Powerline prompt
    // separator). Real terminals render it single-width, so this must use
    // width() rather than width_cjk() (which treats Ambiguous-category
    // characters, including all of Private Use, as double-width) or the
    // cursor column lands one character short, truncating the captured word.
    let line = "\u{e0b0} world";
    assert_eq!(extract_word_before_cursor(line, 7), Some("world".to_string()));
  }

  #[test]
  fn injects_cursor_word_regexp_when_enabled() {
    let last_command_outputs = vec![
      "%100".to_string(),
      "hello world\n".to_string(),
      "@thumbs-cursor-word enabled\n".to_string(),
      "%106:0:24:0:0:nope:0:0\n%98:0:24:0:0:active:11:0\n%107:0:24:0:0:nope:0:0\n".to_string(),
    ];
    let mut executor = TestShell::new(last_command_outputs);
    let mut swapper = Swapper::new(
      Box::new(&mut executor),
      "".to_string(),
      "".to_string(),
      "".to_string(),
      "".to_string(),
      false,
    );

    swapper.capture_active_pane();
    swapper.execute_thumbs();

    let executed = executor.last_executed().unwrap();
    let pane_command = executed.last().unwrap();

    assert!(pane_command.contains("--cursor-word-regexp '\\S*world\\S*'"));
  }

  #[test]
  fn escapes_regex_metacharacters_in_cursor_word() {
    // A half-typed path/version string like "1.91.0" contains a regex
    // metacharacter ('.'); left unescaped, "1.91.0" as a pattern would also
    // match unrelated strings like "1X91X0" instead of literal "1.91.0".
    let last_command_outputs = vec![
      "%100".to_string(),
      "1.91.0\n".to_string(),
      "@thumbs-cursor-word enabled\n".to_string(),
      "%106:0:24:0:0:nope:0:0\n%98:0:24:0:0:active:6:0\n%107:0:24:0:0:nope:0:0\n".to_string(),
    ];
    let mut executor = TestShell::new(last_command_outputs);
    let mut swapper = Swapper::new(
      Box::new(&mut executor),
      "".to_string(),
      "".to_string(),
      "".to_string(),
      "".to_string(),
      false,
    );

    swapper.capture_active_pane();
    swapper.execute_thumbs();

    let executed = executor.last_executed().unwrap();
    let pane_command = executed.last().unwrap();

    assert!(pane_command.contains("--cursor-word-regexp '\\S*1\\.91\\.0\\S*'"));
  }

  #[test]
  fn escapes_literal_single_quote_in_cursor_word_for_shell() {
    // Since the cursor word can now contain any non-blank character
    // (including a literal '), and the built regex is spliced into a
    // single-quoted shell string, a bare "'" would prematurely close that
    // string and corrupt (or inject into) the command tmux runs.
    let last_command_outputs = vec![
      "%100".to_string(),
      "it's\n".to_string(),
      "@thumbs-cursor-word enabled\n".to_string(),
      "%106:0:24:0:0:nope:0:0\n%98:0:24:0:0:active:4:0\n%107:0:24:0:0:nope:0:0\n".to_string(),
    ];
    let mut executor = TestShell::new(last_command_outputs);
    let mut swapper = Swapper::new(
      Box::new(&mut executor),
      "".to_string(),
      "".to_string(),
      "".to_string(),
      "".to_string(),
      false,
    );

    swapper.capture_active_pane();
    swapper.execute_thumbs();

    let executed = executor.last_executed().unwrap();
    let pane_command = executed.last().unwrap();

    // Decodes (per POSIX single-quote escaping) to: '\S*it's\S*'
    assert!(pane_command.contains("--cursor-word-regexp '\\S*it'\\''s\\S*'"));
  }

  #[test]
  fn no_regexp_when_option_unset() {
    let last_command_outputs = vec![
      "%100".to_string(),
      "".to_string(),
      "%106:0:24:0:0:nope:0:0\n%98:0:24:0:0:active:11:0\n%107:0:24:0:0:nope:0:0\n".to_string(),
    ];
    let mut executor = TestShell::new(last_command_outputs);
    let mut swapper = Swapper::new(
      Box::new(&mut executor),
      "".to_string(),
      "".to_string(),
      "".to_string(),
      "".to_string(),
      false,
    );

    swapper.capture_active_pane();
    swapper.execute_thumbs();

    let executed = executor.last_executed().unwrap();
    let pane_command = executed.last().unwrap();

    assert!(!pane_command.contains("\\w*"));
  }

  #[test]
  fn no_regexp_when_in_copy_mode() {
    let last_command_outputs = vec![
      "%100".to_string(),
      "@thumbs-cursor-word enabled\n".to_string(),
      "%106:1:24:1:0:nope:0:0\n%98:1:24:1:0:active:11:0\n%107:1:24:1:0:nope:0:0\n".to_string(),
    ];
    let mut executor = TestShell::new(last_command_outputs);
    let mut swapper = Swapper::new(
      Box::new(&mut executor),
      "".to_string(),
      "".to_string(),
      "".to_string(),
      "".to_string(),
      false,
    );

    swapper.capture_active_pane();
    swapper.execute_thumbs();

    let executed = executor.last_executed().unwrap();
    let pane_command = executed.last().unwrap();

    assert!(!pane_command.contains("\\w*"));
  }

  #[test]
  fn deletes_word_before_cursor_on_upcase_cursor_word_match() {
    let last_command_outputs = vec!["".to_string(), "".to_string()];
    let mut executor = TestShell::new(last_command_outputs);

    let user_command = "echo \"{}\"".to_string();
    let upcase_command = "open \"{}\"".to_string();
    let multi_command = "open \"{}\"".to_string();
    let mut swapper = Swapper::new(
      Box::new(&mut executor),
      "".to_string(),
      user_command,
      upcase_command,
      multi_command,
      false,
    );

    swapper.active_pane_id = Some("%98".to_string());
    swapper.cursor_word = Some("ma".to_string());
    swapper.content = Some(format!(
      "{do_upcase}:{pattern}:{thumb_text}",
      do_upcase = true,
      pattern = "cursor_word",
      thumb_text = "main.rs",
    ));

    swapper.execute_command();

    assert_eq!(executor.history.len(), 2);

    let delete_call = &executor.history[0];
    assert_eq!(delete_call[0..4], ["tmux", "send-keys", "-t", "%98"]);
    assert_eq!(&delete_call[4..], &["BSpace", "BSpace"]);
  }

  #[test]
  fn no_delete_when_upcase_match_is_not_cursor_word() {
    let last_command_outputs = vec!["".to_string()];
    let mut executor = TestShell::new(last_command_outputs);

    let user_command = "echo \"{}\"".to_string();
    let upcase_command = "open \"{}\"".to_string();
    let multi_command = "open \"{}\"".to_string();
    let mut swapper = Swapper::new(
      Box::new(&mut executor),
      "".to_string(),
      user_command,
      upcase_command,
      multi_command,
      false,
    );

    swapper.active_pane_id = Some("%98".to_string());
    swapper.cursor_word = Some("ma".to_string());
    swapper.content = Some(format!(
      "{do_upcase}:{pattern}:{thumb_text}",
      do_upcase = true,
      pattern = "path",
      thumb_text = "main.rs",
    ));

    swapper.execute_command();

    // Only the final bash eval call happens, no preceding send-keys deletion.
    assert_eq!(executor.history.len(), 1);
  }
}

fn app_args<'a>() -> clap::ArgMatches<'a> {
  App::new("tmux-thumbs")
    .version(crate_version!())
    .about("A lightning fast version of tmux-fingers, copy/pasting tmux like vimium/vimperator")
    .arg(
      Arg::with_name("dir")
        .help("Directory where to execute thumbs")
        .long("dir")
        .default_value(""),
    )
    .arg(
      Arg::with_name("command")
        .help("Command to execute after choose a hint")
        .long("command")
        .default_value("tmux set-buffer -- \"{}\" && tmux display-message \"Copied {}\""),
    )
    .arg(
      Arg::with_name("upcase_command")
        .help("Command to execute after choose a hint, in upcase")
        .long("upcase-command")
        .default_value("tmux set-buffer -- \"{}\" && tmux paste-buffer && tmux display-message \"Copied {}\""),
    )
    .arg(
      Arg::with_name("multi_command")
        .help("Command to execute after choose multiple hints")
        .long("multi-command")
        .default_value("tmux set-buffer -- \"{}\" && tmux paste-buffer && tmux display-message \"Multi copied {}\""),
    )
    .arg(
      Arg::with_name("osc52")
        .help("Print OSC52 copy escape sequence in addition to running the pick command")
        .long("osc52")
        .short("o"),
    )
    .get_matches()
}

fn main() -> std::io::Result<()> {
  let args = app_args();
  let dir = args.value_of("dir").unwrap();
  let command = args.value_of("command").unwrap();
  let upcase_command = args.value_of("upcase_command").unwrap();
  let multi_command = args.value_of("multi_command").unwrap();
  let osc52 = args.is_present("osc52");

  if dir.is_empty() {
    panic!("Invalid tmux-thumbs execution. Are you trying to execute tmux-thumbs directly?")
  }

  let mut executor = RealShell::new();
  let mut swapper = Swapper::new(
    Box::new(&mut executor),
    dir.to_string(),
    command.to_string(),
    upcase_command.to_string(),
    multi_command.to_string(),
    osc52,
  );

  swapper.capture_active_pane();
  swapper.execute_thumbs();
  swapper.swap_panes();
  swapper.resize_pane();
  swapper.wait_thumbs();
  swapper.retrieve_content();
  swapper.destroy_content();
  swapper.execute_command();

  Ok(())
}
