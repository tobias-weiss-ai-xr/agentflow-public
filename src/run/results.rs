//! Query commands: `af attach` (tail a task log) and `af results`.

use crate::config::{Config, Settings, TaskState};
use crate::state::Store;
use std::io::Write;
use std::time::Duration;

/// `af attach <id>`: tail a task's log until its terminal state.
pub fn attach(st: &Settings, id: &str) -> i32 {
    let store = Store::new(st.state_dir.clone());
    let log_path = store.log_dir().join(format!("{id}.log"));
    let mut pos: u64 = 0;
    loop {
        if let Ok(meta) = std::fs::metadata(&log_path) {
            let len = meta.len();
            if len > pos {
                if let Ok(mut f) = std::fs::File::open(&log_path) {
                    use std::io::{Read, Seek, SeekFrom};
                    let _ = f.seek(SeekFrom::Start(pos));
                    let mut buf = Vec::new();
                    let _ = f.read_to_end(&mut buf);
                    pos = len;
                    let _ = std::io::stdout().write_all(&buf);
                    let _ = std::io::stdout().flush();
                }
            }
        }
        match store.load().get(id).map(|s| &s.state) {
            Some(TaskState::Done) => return 0,
            Some(TaskState::Failed) => return 1,
            _ => std::thread::sleep(Duration::from_millis(300)),
        }
    }
}

/// `af api results --task <id>`: status + last log lines.
pub fn results(cfg: &Config, st: &Settings, id: &str) -> String {
    if !cfg.by_id.contains_key(id) {
        return format!("no such task: {id}");
    }
    let store = Store::new(st.state_dir.clone());
    let mut out = String::new();
    if let Some(s) = store.load().get(id) {
        out.push_str(&format!(
            "task {id}: state={:?}, attempts={}\n",
            s.state, s.attempts
        ));
        if let Some(e) = &s.last_error {
            out.push_str(&format!("result: {e}\n"));
        }
    }
    if let Ok(content) = std::fs::read_to_string(store.log_dir().join(format!("{id}.log"))) {
        let all: Vec<&str> = content.lines().collect();
        out.push_str("--- last log lines ---\n");
        for l in all[all.len().saturating_sub(30)..].iter() {
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}
