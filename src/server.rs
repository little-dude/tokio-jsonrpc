// Copyright 2017 tokio-jsonrpc Developers
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! The [`Server`](trait.Server.html) trait and helpers.
//!
//! The `Server` trait for the use by the [`Endpoint`](../endpoint/struct.Endpoint.html) is defined
//! here. Furthermore, some helpers for convenient creation and composition of servers are
//! available. Note that not all of these helpers are necessarily zero-cost, at least at this time.

use futures::{Future, IntoFuture};
use serde::Serialize;
use serde_json::{Value, to_value};

use endpoint::ServerCtl;
use message::RPCError;

/// The server endpoint.
///
/// This is usually implemented by the end application and provides the actual functionality of the
/// RPC server. It allows composition of more servers together.
///
/// The default implementations of the callbacks return None, indicating that the given method is
/// not known. It allows implementing only RPCs or only notifications without having to worry about
/// the other callback. If you want a server that does nothing at all, use
/// [`Empty`](struct.Empty.html).
pub trait Server {
    /// The successfull result of the RPC call.
    type Success: Serialize;
    /// The result of the RPC call
    ///
    /// Once the future resolves, the value or error is sent to the client as the reply. The reply
    /// is wrapped automatically.
    type RPCCallResult: IntoFuture<Item = Self::Success, Error = RPCError> + 'static;
    /// The result of the RPC call.
    ///
    /// As the client doesn't expect anything in return, both the success and error results are
    /// thrown away and therefore (). However, it still makes sense to distinguish success and
    /// error.
    type NotificationResult: IntoFuture<Item = (), Error = ()> + 'static;
    /// Called when the client requests something.
    ///
    /// This is a callback from the [endpoint](struct.Endpoint.html) when the client requests
    /// something. If the method is unknown, it shall return `None`. This allows composition of
    /// servers.
    ///
    /// Conversion of parameters and handling of errors is up to the implementer of this trait.
    fn rpc(&self, _ctl: &ServerCtl, _method: &str, _params: &Option<Value>)
           -> Option<Self::RPCCallResult> {
        None
    }
    /// Called when the client sends a notification.
    ///
    /// This is a callback from the [endpoint](struct.Endpoint.html) when the client requests
    /// something. If the method is unknown, it shall return `None`. This allows composition of
    /// servers.
    ///
    /// Conversion of parameters and handling of errors is up to the implementer of this trait.
    fn notification(&self, _ctl: &ServerCtl, _method: &str, _params: &Option<Value>)
                    -> Option<Self::NotificationResult> {
        None
    }
    /// Called when the endpoint is initialized.
    ///
    /// It provides a default empty implementation, which can be overriden to hook onto the
    /// initialization.
    fn initialized(&self, _ctl: &ServerCtl) {}
}

/// A RPC server that knows no methods.
///
/// You can use this if you want to have a client-only [Endpoint](struct.Endpoint.html). It simply
/// terminates the server part right away. Or, more conveniently, use `Endpoint`'s
/// [`client_only`](struct.Endpoint.html#method.client_only) method.
pub struct Empty;

impl Server for Empty {
    type Success = ();
    type RPCCallResult = Result<(), RPCError>;
    type NotificationResult = Result<(), ()>;
    fn initialized(&self, ctl: &ServerCtl) {
        ctl.terminate();
    }
}

/// An RPC server wrapper with dynamic dispatch.
///
/// This server wraps another server and converts it into a common ground, so multiple different
/// servers can be used as trait objects. Basically, it boxes the futures it returns and converts
/// the result into `serde_json::Value`. It can then be used together with
/// [`ServerChain`](struct.ServerChain.html) easilly. Note that this conversion incurs
/// runtime costs.
pub struct AbstractServer<S: Server>(S);

impl<S: Server> AbstractServer<S> {
    /// Wraps another server into an abstract server.
    pub fn new(server: S) -> Self {
        AbstractServer(server)
    }
    /// Unwraps the abstract server and provides the one inside back.
    pub fn into_inner(self) -> S {
        self.0
    }
}

impl<S: Server> Server for AbstractServer<S> {
    type Success = Value;
    type RPCCallResult = Box<Future<Item = Value, Error = RPCError>>;
    type NotificationResult = Box<Future<Item = (), Error = ()>>;
    fn rpc(&self, ctl: &ServerCtl, method: &str, params: &Option<Value>)
           -> Option<Self::RPCCallResult> {
        self.0
            .rpc(ctl, method, params)
            .map(|f| -> Box<Future<Item = Value, Error = RPCError>> {
                let future = f.into_future()
                    .map(|result| {
                        to_value(result)
                            .expect("Your result type is not convertible to JSON, which is a bug")
                    });
                Box::new(future)
            })
    }
    fn notification(&self, ctl: &ServerCtl, method: &str, params: &Option<Value>)
                    -> Option<Self::NotificationResult> {
        // It seems the type signature is computed from inside the closure and it doesn't fit on
        // the outside, so we need to declare it manually :-(
        self.0
            .notification(ctl, method, params)
            .map(|f| -> Box<Future<Item = (), Error = ()>> { Box::new(f.into_future()) })
    }
    fn initialized(&self, ctl: &ServerCtl) {
        self.0.initialized(ctl)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    /// Check the empty server is somewhat sane.
    #[test]
    fn empty() {
        let server = Empty;
        let (ctl, dropped, _killed) = ServerCtl::new_test();
        // As we can't reasonably check all possible method names, do so for just a bunch
        for method in ["method", "notification", "check"].iter() {
            assert!(server.rpc(&ctl, method, &None).is_none());
            assert!(server.notification(&ctl, method, &None).is_none());
        }
        // It terminates the ctl on the server side on initialization
        server.initialized(&ctl);
        dropped.wait().unwrap();
    }

    /// A server that logs what has been called.
    #[derive(Default, Debug, PartialEq)]
    struct LogServer {
        serial: Cell<usize>,
        rpc: RefCell<Vec<usize>>,
        notification: RefCell<Vec<usize>>,
        initialized: RefCell<Vec<usize>>,
    }

    impl LogServer {
        fn update(&self, what: &RefCell<Vec<usize>>) {
            let serial = self.serial.get() + 1;
            self.serial.set(serial);
            what.borrow_mut().push(serial);
        }
    }

    impl Server for LogServer {
        type Success = bool;
        type RPCCallResult = Result<bool, RPCError>;
        type NotificationResult = Result<(), ()>;
        fn rpc(&self, _ctl: &ServerCtl, method: &str, params: &Option<Value>)
               -> Option<Self::RPCCallResult> {
            self.update(&self.rpc);
            assert!(params.is_none());
            match method {
                "test" => Some(Ok(true)),
                _ => None,
            }
        }
        fn notification(&self, _ctl: &ServerCtl, method: &str, params: &Option<Value>)
                        -> Option<Self::NotificationResult> {
            self.update(&self.notification);
            assert!(params.is_none());
            match method {
                "notification" => Some(Ok(())),
                _ => None,
            }
        }
        fn initialized(&self, _ctl: &ServerCtl) {
            self.update(&self.initialized);
        }
    }

    /// Testing of the abstract server
    ///
    /// Just checking the data gets through and calling everything, there's nothing much to test
    /// anyway.
    #[test]
    fn abstract_server() {
        let log_server = LogServer::default();
        let abstract_server = AbstractServer::new(log_server);
        let (ctl, _, _) = ServerCtl::new_test();
        let rpc_result = abstract_server.rpc(&ctl, "test", &None)
            .unwrap()
            .wait()
            .unwrap();
        assert_eq!(Value::Bool(true), rpc_result);
        abstract_server.notification(&ctl, "notification", &None)
            .unwrap()
            .wait()
            .unwrap();
        assert!(abstract_server.rpc(&ctl, "another", &None).is_none());
        assert!(abstract_server.notification(&ctl, "another", &None).is_none());
        abstract_server.initialized(&ctl);
        let log_server = abstract_server.into_inner();
        let expected = LogServer {
            serial: Cell::new(5),
            rpc: RefCell::new(vec![1, 3]),
            notification: RefCell::new(vec![2, 4]),
            initialized: RefCell::new(vec![5]),
        };
        assert_eq!(expected, log_server);
    }
}