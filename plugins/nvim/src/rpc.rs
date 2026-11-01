//! Neovim's msgpack-RPC over `nvim --embed`'s stdio.

use std::io::{self, BufReader, BufWriter, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use rmpv::Value;

/// One message from Neovim.
#[derive(Debug)]
pub enum Msg {
    Response {
        id: u32,
        err: Option<String>,
        result: Value,
    },
    Notify {
        method: String,
        params: Vec<Value>,
    },
    Request {
        id: u32,
    },
}

/// Writes requests; replies arrive through [`read_loop`].
pub struct Nvim {
    pub child: Child,
    stdin: BufWriter<ChildStdin>,
    next_id: u32,
}

impl Nvim {
    /// Starts Neovim. It waits for `nvim_ui_attach` before loading anything.
    pub fn spawn(args: &[String]) -> io::Result<(Self, ChildStdout)> {
        let mut child = Command::new("nvim")
            .arg("--embed")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let stdin = BufWriter::new(child.stdin.take().expect("piped"));
        let stdout = child.stdout.take().expect("piped");
        Ok((
            Self {
                child,
                stdin,
                next_id: 1,
            },
            stdout,
        ))
    }

    /// Queues a request and returns its id. A dead Neovim surfaces as EOF on
    /// the reader, so a failed write has nothing further to report.
    pub fn request(&mut self, method: &str, args: Vec<Value>) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        let msg = Value::Array(vec![0.into(), id.into(), method.into(), Value::Array(args)]);
        let _ = rmpv::encode::write_value(&mut self.stdin, &msg);
        id
    }

    /// `nvim_exec_lua(code, args)`.
    pub fn lua(&mut self, code: &str, args: Vec<Value>) -> u32 {
        self.request("nvim_exec_lua", vec![code.into(), Value::Array(args)])
    }

    /// Answers a request Neovim sent us.
    pub fn respond_error(&mut self, id: u32) {
        let msg = Value::Array(vec![
            1.into(),
            id.into(),
            "phux-nvim serves no requests".into(),
            Value::Nil,
        ]);
        let _ = rmpv::encode::write_value(&mut self.stdin, &msg);
    }

    pub fn flush(&mut self) {
        let _ = self.stdin.flush();
    }
}

/// Decodes messages until EOF, handing each to `sink`.
pub fn read_loop(stdout: ChildStdout, mut sink: impl FnMut(Msg)) {
    let mut r = BufReader::with_capacity(64 * 1024, stdout);
    while let Ok(v) = rmpv::decode::read_value(&mut r) {
        if let Some(m) = parse(v) {
            sink(m);
        }
    }
}

fn parse(v: Value) -> Option<Msg> {
    let Value::Array(mut a) = v else { return None };
    match (a.first()?.as_u64()?, a.len()) {
        (0, 4) => Some(Msg::Request {
            id: u32::try_from(a[1].as_u64()?).ok()?,
        }),
        (1, 4) => {
            let result = a.pop()?;
            let err = a.pop()?;
            let id = u32::try_from(a[1].as_u64()?).ok()?;
            let err = (!err.is_nil()).then(|| err.to_string());
            Some(Msg::Response { id, err, result })
        }
        (2, 3) => {
            let Value::Array(params) = a.pop()? else {
                return None;
            };
            Some(Msg::Notify {
                method: a.pop()?.as_str()?.to_owned(),
                params,
            })
        }
        _ => None,
    }
}

/// A window handle: an ext value wrapping an int, or a plain int.
pub fn handle(v: &Value) -> Option<i64> {
    match v {
        Value::Ext(_, bytes) => rmpv::decode::read_value(&mut bytes.as_slice())
            .ok()?
            .as_i64(),
        other => other.as_i64(),
    }
}

/// A number Neovim may send as float or int.
pub fn number(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_i64().map(|n| n as f64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_are_ext_or_int() {
        assert_eq!(handle(&Value::Ext(1, vec![0xcd, 0x03, 0xe8])), Some(1000));
        assert_eq!(handle(&Value::from(-1)), Some(-1));
        assert_eq!(handle(&Value::Nil), None);
    }

    #[test]
    fn messages_parse_by_kind() {
        let n = Value::Array(vec![
            2.into(),
            "redraw".into(),
            Value::Array(vec![1.into()]),
        ]);
        assert!(
            matches!(parse(n), Some(Msg::Notify { method, params }) if method == "redraw" && params.len() == 1)
        );
        let r = Value::Array(vec![1.into(), 7.into(), Value::Nil, true.into()]);
        assert!(matches!(
            parse(r),
            Some(Msg::Response {
                id: 7,
                err: None,
                ..
            })
        ));
        assert!(parse(Value::from(3)).is_none());
    }
}
