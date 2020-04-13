use std::{
    io::{self, BufRead, BufReader, Error, ErrorKind},
    str::FromStr,
};

use nom::{
    bytes::streaming::{take_until, take_while, take_while1},
    character::is_space,
    character::streaming::crlf,
    sequence::tuple,
    Err::Incomplete,
    IResult,
};

use crate::{ServerInfo, Stream};

// Protocol
const INFO: &[u8] = b"INFO";
const MSG: &[u8] = b"MSG";
const PING: &[u8] = b"PING";
const PONG: &[u8] = b"PONG";
const ERR: &[u8] = b"-ERR";

#[inline]
fn is_valid_op_char(c: u8) -> bool {
    (c >= 0x41 && c <= 0x5A) || c == b'-' || c == b'+'
}

pub(crate) fn parse_control_op<R: BufRead>(mut reader: R) -> io::Result<ControlOp> {
    // This should not do a malloc here so this should be ok.
    let mut buf = Vec::new();
    let (input, start_len, (op, args)) = {
        if let Some((input, start_len, (op, args))) = {
            let input = reader.fill_buf()?;
            let start_len = input.len();
            let r = tuple((take_while1(is_valid_op_char), control_args))(input);
            match r {
                Ok((input, (op, args))) => Some((input, start_len, (op, args))),
                Err(Incomplete(_)) => None,
                _ => return Err(parse_error()),
            }
        } {
            (input, start_len, (op, args))
        } else {
            reader.read_until(b'\n', &mut buf)?;
            let r = tuple((take_while1(is_valid_op_char), control_args))(&mut buf);
            let (input, (op, args)) = if let Ok((input, (op, args))) = r {
                (input, (op, args))
            } else {
                // Check for EOF, this happens on close of connection.
                if buf.is_empty() {
                    return Err(Error::new(ErrorKind::UnexpectedEof, "socket closed"));
                }
                return Err(parse_error());
            };
            (input, 0, (op, args))
        }
    };

    let op = match op {
        MSG => parse_msg_args(args)?,
        INFO => parse_info(args)?,
        PING => ControlOp::Ping,
        PONG => ControlOp::Pong,
        ERR => parse_err(args),
        _ => ControlOp::Unknown(String::from_utf8_lossy(op).to_string()),
    };

    // Make sure to consume the bytes from the underlying buffer.
    let stop_len = input.len();
    reader.consume(start_len - stop_len);

    Ok(op)
}

fn parse_msg_args(args: &[u8]) -> io::Result<ControlOp> {
    let a = String::from_utf8_lossy(args);
    // subject sid <reply> msg_len
    // TODO(dlc) - convert to nom.
    let args: Vec<&str> = a.split(' ').collect();
    let (subject, len_index, reply) = match args.len() {
        3 => (args[0], 2, None),
        4 => (args[0], 3, Some(args[2].to_owned())),
        _ => return Err(parse_error()),
    };
    let sid = if let Ok(sid) = usize::from_str(args[1]) {
        sid
    } else {
        return Err(parse_error());
    };
    let mlen = if let Ok(mlen) = u32::from_str(args[len_index]) {
        mlen
    } else {
        return Err(parse_error());
    };
    let m = MsgArgs {
        subject: subject.to_owned(),
        reply,
        //        data: Vec::with_capacity(mlen as usize),
        mlen,
        sid,
    };
    Ok(ControlOp::Msg(m))
}

fn parse_error() -> Error {
    Error::new(ErrorKind::InvalidInput, "parsing error")
}

fn parse_err(args: &[u8]) -> ControlOp {
    let err_description = String::from_utf8_lossy(args);
    let err_description = err_description.trim_matches('\'');
    ControlOp::Err(err_description.to_string())
}

pub(crate) fn expect_info(reader: &mut BufReader<Stream>) -> io::Result<ServerInfo> {
    let op = parse_control_op(reader)?;

    if let ControlOp::Info(info) = op {
        Ok(info)
    } else {
        Err(Error::new(ErrorKind::Other, "INFO proto not found"))
    }
}

/*
// This takes a TcpStream instead of a buffered reader because
// we may need to
pub(crate) fn expect_info(stream: &mut TcpStream) -> io::Result<ServerInfo> {
    fn read_line(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
        fn ends_with_crlf(buf: &[u8]) -> bool {
            buf.len() >= 2 && buf[buf.len() - 2] == b'\r' && buf[buf.len() - 1] == b'\n'
        }

        let mut buf = vec![];
        while !ends_with_crlf(&buf) {
            let mut read_buf = [0];
            if 1 == stream.read(&mut read_buf)? {
                buf.push(read_buf[0]);
            } else {
                break;
            }
        }
        if buf.len() <= 2 {
            return Err(Error::new(ErrorKind::UnexpectedEof, "socket closed"));
        }
        assert_eq!(buf.pop().unwrap(), b'\n');
        assert_eq!(buf.pop().unwrap(), b'\r');
        Ok(buf)
    }

    let line = read_line(stream)?;

    if line.starts_with(b"INFO ") {
        Ok(serde_json::from_slice(&line["INFO ".len()..])?)
    } else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "expected INFO block but received {}",
                String::from_utf8(line).unwrap_or_else(|_| "invalid data".to_string())
            ),
        ));
    }
}
*/

const CRLF: &str = "\r\n";

#[inline]
fn control_args(input: &[u8]) -> IResult<&[u8], &[u8]> {
    let (remainder, (_, args, _)) = tuple((take_while(is_space), take_until(CRLF), crlf))(input)?;
    Ok((remainder, args))
}

fn parse_info(input: &[u8]) -> io::Result<ControlOp> {
    let info = serde_json::from_slice(input)?;
    Ok(ControlOp::Info(info))
}

#[derive(Debug)]
pub(crate) struct MsgArgs {
    pub(crate) subject: String,
    pub(crate) reply: Option<String>,
    pub(crate) mlen: u32,
    pub(crate) sid: usize,
}

#[derive(Debug)]
pub(crate) enum ControlOp {
    Msg(MsgArgs),
    Info(ServerInfo),
    Ping,
    Pong,
    Err(String),
    Unknown(String),
}
