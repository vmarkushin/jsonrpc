use cors;

use std::ops::Deref;
use std::sync::{Arc, Mutex};
use std::io::{self, Read, Write};
use unicase::UniCase;
use hyper::{mime, server, Next, Encoder, Decoder};
use hyper::header::{Headers, Allow, ContentType, AccessControlAllowHeaders};
use hyper::method::Method;
use hyper::net::HttpStream;
use hyper::header::AccessControlAllowOrigin;
use jsonrpc::IoHandler;
use request_response::{Request, Response};
use hosts_validator::is_host_header_valid;

/// PanicHandling function
pub struct PanicHandler {
	/// Actual handler
	pub handler: Arc<Mutex<Option<Box<Fn() -> () + Send + 'static>>>>
}

/// jsonrpc http request handler.
pub struct ServerHandler {
	panic_handler: PanicHandler,
	jsonrpc_handler: Arc<IoHandler>,
	cors_domains: Option<Vec<AccessControlAllowOrigin>>,
	allowed_hosts: Option<Vec<String>>,
	request: Request,
	response: Response,
}

impl Drop for ServerHandler {
	fn drop(&mut self) {
		if ::std::thread::panicking() {
			let handler = self.panic_handler.handler.lock().unwrap();
			if let Some(ref h) = *handler.deref() {
				h();
			}
		}
	}
}

impl ServerHandler {
	/// Create new request handler.
	pub fn new(
		jsonrpc_handler: Arc<IoHandler>,
		cors_domains: Option<Vec<AccessControlAllowOrigin>>,
		allowed_hosts: Option<Vec<String>>,
		panic_handler: PanicHandler
	) -> Self {
		ServerHandler {
			panic_handler: panic_handler,
			jsonrpc_handler: jsonrpc_handler,
			cors_domains: cors_domains,
			allowed_hosts: allowed_hosts,
			request: Request::empty(),
			response: Response::method_not_allowed(),
		}
	}

	fn response_headers(&self, origin: &Option<String>) -> Headers {
		let mut headers = Headers::new();
		headers.set(self.response.content_type.clone());
		headers.set(Allow(vec![
			Method::Options,
			Method::Post,
		]));
		headers.set(AccessControlAllowHeaders(vec![
			UniCase("origin".to_owned()),
			UniCase("content-type".to_owned()),
			UniCase("accept".to_owned()),
		]));

		if let Some(cors_domain) = cors::get_cors_header(&self.cors_domains, origin) {
			headers.set(cors_domain);
		}

		headers
	}

	fn is_json(&self, content_type: Option<&ContentType>) -> bool {
		if let Some(&ContentType(mime::Mime(mime::TopLevel::Application, mime::SubLevel::Json, _))) = content_type {
			true
		} else {
			false
		}
	}
}

impl server::Handler<HttpStream> for ServerHandler {
	fn on_request(&mut self, request: server::Request<HttpStream>) -> Next {
		// Validate host
		if let Some(ref allowed_hosts) = self.allowed_hosts {
			if !is_host_header_valid(&request, allowed_hosts) {
				self.response = Response::host_not_allowed();
				return Next::write();
			}
		}

		// Read origin
		self.request.origin = cors::read_origin(&request);

		match *request.method() {
			// Don't validate content type on options
			Method::Options => {
				self.response = Response::empty();
				Next::write()
			},
			// Validate the ContentType header
			// to prevent Cross-Origin XHRs with text/plain
			Method::Post if self.is_json(request.headers().get::<ContentType>()) => {
				Next::read()
			},
			Method::Post => {
				// Just return error
				self.response = Response::unsupported_content_type();
				Next::write()
			},
			_ => {
				self.response = Response::method_not_allowed();
				Next::write()
			}
		}
	}

	/// This event occurs each time the `Request` is ready to be read from.
	fn on_request_readable(&mut self, decoder: &mut Decoder<HttpStream>) -> Next {
		match decoder.read_to_string(&mut self.request.content) {
			Ok(0) => {
				self.response = Response::ok(self.jsonrpc_handler.handle_request(&self.request.content).unwrap_or_else(String::new));
				Next::write()
			}
			Ok(_) => {
				Next::read()
			}
			Err(e) => match e.kind() {
				io::ErrorKind::WouldBlock => Next::read(),
				_ => {
					Next::end()
				}
			}
		}
	}

	/// This event occurs after the first time this handled signals `Next::write()`.
	fn on_response(&mut self, response: &mut server::Response) -> Next {
		*response.headers_mut() = self.response_headers(&self.request.origin);
		response.set_status(self.response.code);
		Next::write()
	}

	/// This event occurs each time the `Response` is ready to be written to.
	fn on_response_writable(&mut self, encoder: &mut Encoder<HttpStream>) -> Next {
		let bytes = self.response.content.as_bytes();
		if bytes.len() == self.response.write_pos {
			return Next::end();
		}

		match encoder.write(&bytes[self.response.write_pos..]) {
			Ok(0) => {
				Next::write()
			}
			Ok(bytes) => {
				self.response.write_pos += bytes;
				Next::write()
			}
			Err(e) => match e.kind() {
				io::ErrorKind::WouldBlock => Next::write(),
				_ => {
					//trace!("Write error: {}", e);
					Next::end()
				}
			}
		}
	}
}

