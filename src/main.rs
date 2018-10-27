extern crate futures;
extern crate tokio;
#[macro_use]
extern crate serde_derive;
extern crate serde;
extern crate serde_json;
extern crate birl;

use std::io;
/// Ways in which transactions can fail.
#[derive(Debug, Clone, Serialize)]
enum TransactionError{
	/// An IO Error occured.
	IoError,
	/// Read/Write operations timed out.
	Timeout,
	/// Error during serialization.
	SerializationError,
	/// Input buffer is not valid UTF-8.
	InvalidUtf8(String),
	/// Error string proced by BIRL during parsing.
	ParseError(String),
	/// Error string produced by BIRL during execution.
	RuntimeError(String),
	/// The BIRL interpreter panic'd during execution.
	RuntimePanic,
}

/// Performs a transaction between server and client.
/// 
/// A transaction consists of processing a client-provided input buffer,
/// expected to be a valid program, and producing an output buffer, the output
/// produced by the program. Otherwise, producing an error.
fn transaction(strip: String) -> Result<Vec<u8>, TransactionError>{
	/* Input and output streams for the program.
	 * For now all the input stream does is provide
	 * and endless stream of newlines. Meanwhile,
	 * the output stream will collect whatever the
	 * program produces in a buffer.
	 *
	 * TODO: Allow pre-baked input.
	 */
	struct Input;
	struct Output(Vec<u8>);

	use std::io;
	impl io::Read for Input{
		fn read(&mut self, out: &mut [u8]) -> io::Result<usize>{
			/* No real equivalent to memset() exists in Rust,
			 * so a loop must do. That seems to be the way to
			 * go. */
			let len = out.len();
			for i in 0..len{ out[i] = 0x0a }
			
			Ok(len)
		}
	}
	impl io::Write for Output{
		fn write(&mut self, buf: &[u8]) -> io::Result<usize>{
			/* NOTE: This can grow without bounds within the memory
			 * limits of the thread it's running in. */
			self.0.extend_from_slice(buf);
			Ok(buf.len())
		}

		fn flush(&mut self) -> io::Result<()>{ Ok(()) }
	}

	/* Since we have no guarantees BIRL will not panic during 
	 * runtime, we'll panic guard it for safety, to ensure one
	 * transaction with the interpreter won't bring the server down.
	 *
	 * NOTE: Program abortions may still bring the server down, as
	 * they're not catched by catch_unwind().
	 */
	use std::panic;
	let output = panic::catch_unwind(move || -> Result<_, TransactionError> {
		use birl::context::Context;
		let mut c = Context::new();

		/* Feed the program. */
		strip.lines()
			.try_for_each(|l| c.process_line(l))
			.map_err(|what| TransactionError::ParseError(what))?;


		let _ = c.set_stdin({
			let input = io::BufReader::new(Input);
			Some(Box::new(input))
		});
		let _ = c.set_stdout({
			let output = Output(Vec::new());
			Some(Box::new(output))
		});

		c.start_program();
		Ok(c.set_stdout(None))
	}).map_err(|_| TransactionError::RuntimePanic)??;

	/* Lastly, recover the resulting output buffer and present it. */
	let output = output.expect("stdout option mismatch");
	let output = {
		/* FIXME: Downcast doesn't really work for this type in particular */
		let raw = Box::into_raw(output) as *mut Output;
		unsafe { Box::from_raw(raw) }
	};
	Ok((*output).0)
}

use std::net::SocketAddr;
use std::time::Duration;
struct Settings{
	address: SocketAddr,
	timeout: Duration
}
impl Settings{
	fn defaults() -> Self{
		use std::net::ToSocketAddrs;
		Settings{
			address: "127.0.0.1:25367".to_socket_addrs().unwrap().next().unwrap(),
			timeout: Duration::from_secs(10),
		}
	}
}

fn main(){
	let settings = Settings::defaults();
	let timeout0 = settings.timeout.clone();
	let timeout1 = settings.timeout.clone();

	/* Setup server. */
	use tokio::net::tcp::TcpListener;
	let listener = TcpListener::bind(&settings.address)
		.expect("Could not bind TCP listener");


	/* Setup processing. */
	use futures::{Stream, Future};
	let incoming = listener.incoming().map_err(|_|
		TransactionError::IoError
	).and_then(move |stream|{
		/* Read the incoming stream until either an end of
		 * transmission or a timeout of ten seconds. */
		use tokio::io::read_until;
		use tokio::util::FutureExt;
		use std::io::BufReader;

		let addr = stream.peer_addr();
		let buf = BufReader::new(stream);
		read_until(buf, 4, Vec::new())
			.timeout(timeout0)
			.map_err(move |_|{
				eprintln!("[incoming()] Program read timed out \
						  for connection from {:?}", addr);
				TransactionError::Timeout
			})
			.map(|(buf, buffer)| {
				(
					buf.into_inner(),
					{
						let len = buffer.len();
						buffer.into_iter().take(len - 1).collect::<Vec<_>>()
					}
				)
			})
	}).and_then(|(stream, buffer)|{
		use futures::future;
		future::ok(
			match String::from_utf8(buffer){
				Ok(string) => (stream, transaction(string)),
				Err(what)  => {
					let msg = format!("{:?}", what);
					(stream, Err(TransactionError::InvalidUtf8(msg)))
				}
			}
		)
	}).and_then(|(stream, result)|{ 
		/* Try serializing. */
		serde_json::to_string(&result)
			.map(|json| (stream, json))
			.map_err(|e|{ 
				eprintln!("[incoming()] Could not \
					serialize response: {:?}", e);
				TransactionError::SerializationError
			})
	}).and_then(move |(stream, json)|{
		/* Write the serialized results with a timeout. */
		use tokio::io::write_all;
		use tokio::util::FutureExt;

		let addr = stream.peer_addr();
		write_all(stream, json)
			.timeout(timeout1)
			.map_err(move |_|{
				eprintln!("[incoming()] Program write timed out \
						  for connection from {:?}", addr);
				TransactionError::Timeout
			})
			.map(|(stream, _)| (stream, ()))
	}).for_each(|(stream, buffer)| {
		use futures::future;
		future::ok(())
	}).map_err(|e| eprintln!("[incoming()] Error: {:?}", e));

	/* TODO: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA */
	tokio::run(incoming);
}
