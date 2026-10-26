//! The little HTTP that the Jira and Toggl clients need, behind a trait so they can be
//! tested with canned responses instead of real accounts.

use std::time::Duration;

use base64::Engine as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

#[derive(Clone, Debug)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// A JSON body, for POST and PUT.
    pub body: Option<String>,
}

impl Request {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self { method, url: url.into(), headers: Vec::new(), body: None }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }

    pub fn json(mut self, body: String) -> Self {
        self.body = Some(body);
        self.header("Content-Type", "application/json")
    }
}

#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub body: String,
    /// Seconds to wait before trying again, when the server said so.
    pub retry_after: Option<u64>,
}

impl Response {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Sends a request and returns whatever came back, including error statuses. Only a
/// failure to get any response at all, such as being offline, is an `Err`.
pub trait Transport: Send + Sync {
    fn send(&self, request: Request) -> Result<Response, String>;
}

/// The real thing.
pub struct Http {
    agent: ureq::Agent,
}

impl Default for Http {
    fn default() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
            .into();
        Self { agent }
    }
}

impl Transport for Http {
    fn send(&self, request: Request) -> Result<Response, String> {
        let result = match request.method {
            Method::Get | Method::Delete => {
                let mut builder = match request.method {
                    Method::Get => self.agent.get(&request.url),
                    _ => self.agent.delete(&request.url),
                };
                for (name, value) in &request.headers {
                    builder = builder.header(name, value);
                }
                builder.call()
            }
            Method::Post | Method::Put => {
                let mut builder = match request.method {
                    Method::Post => self.agent.post(&request.url),
                    _ => self.agent.put(&request.url),
                };
                for (name, value) in &request.headers {
                    builder = builder.header(name, value);
                }
                builder.send(request.body.unwrap_or_default())
            }
        };
        let mut response = result.map_err(|e| e.to_string())?;
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse().ok());
        let status = response.status().as_u16();
        let body = response.body_mut().read_to_string().unwrap_or_default();
        Ok(Response { status, body, retry_after })
    }
}

/// The value of an `Authorization` header for basic auth.
pub fn basic_auth(user: &str, password: &str) -> String {
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}")))
}

/// Canned responses for tests. Each response is matched by method and a substring of the
/// URL, first match first, and every request is recorded.
#[cfg(feature = "fake")]
pub mod fake {
    use std::sync::Mutex;

    use super::{Method, Request, Response, Transport};

    #[derive(Default)]
    pub struct Fake {
        routes: Mutex<Vec<(Method, String, Response)>>,
        pub requests: Mutex<Vec<Request>>,
    }

    impl Fake {
        pub fn new() -> Self {
            Self::default()
        }

        /// Answers the next matching request. Routes are used once, in order.
        pub fn respond(&self, method: Method, url_contains: &str, status: u16, body: &str) -> &Self {
            self.routes.lock().unwrap().push((
                method,
                url_contains.to_owned(),
                Response { status, body: body.to_owned(), retry_after: None },
            ));
            self
        }

        pub fn respond_with(&self, method: Method, url_contains: &str, response: Response) -> &Self {
            self.routes.lock().unwrap().push((method, url_contains.to_owned(), response));
            self
        }

        pub fn urls(&self) -> Vec<String> {
            self.requests.lock().unwrap().iter().map(|r| r.url.clone()).collect()
        }
    }

    impl Transport for Fake {
        fn send(&self, request: Request) -> Result<Response, String> {
            self.requests.lock().unwrap().push(request.clone());
            let mut routes = self.routes.lock().unwrap();
            let position = routes
                .iter()
                .position(|(method, url, _)| *method == request.method && request.url.contains(url.as_str()))
                .ok_or_else(|| format!("no canned response for {:?} {}", request.method, request.url))?;
            Ok(routes.remove(position).2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_auth_encodes_user_and_password() {
        assert_eq!(basic_auth("me@example.com", "secret"), "Basic bWVAZXhhbXBsZS5jb206c2VjcmV0");
    }
}
