//! Channel method dispatch — the bodies of the `send`/`recv`/
//! `try_recv` arms lifted out of `eval_method_call`. Receivers are
//! `Value::Sender` and `Value::Receiver`.

use crate::ast::*;
use crate::token::Span;

use super::value::{EnumData, Value};

impl<'a> super::Interpreter<'a> {
    pub(super) fn try_eval_channel_method(
        &mut self,
        method: &str,
        obj: &Value,
        args: &[CallArg],
        _span: &Span,
    ) -> Option<Value> {
        match method {
            "send" => {
                let val = args
                    .first()
                    .map(|a| self.eval_expr_inner(&a.value))
                    .unwrap_or(Value::Unit);
                if let Value::Sender(ref buf) = obj {
                    let mut q = buf.queue.lock().unwrap();
                    // Bounded backpressure (B-2026-08-22-16). The tree-walk
                    // interpreter is single-threaded, so there is no peer that
                    // could drain the queue while a sender waits — parking
                    // would deadlock outright. A full `send` therefore panics,
                    // which is the v1 `Block` -> `FailFast` collapse
                    // `runtime/stdlib/bounded_channel.kara` documents, and
                    // `send` returns unit so there is no Err channel to use
                    // instead. The COMPILED runtime, which has real threads,
                    // parks the sender on a condvar and only panics where
                    // blocking is likewise impossible (sequential wasm).
                    if buf.capacity != 0 && q.len() >= buf.capacity {
                        drop(q);
                        return Some(self.record_runtime_error(
                            format!("send on a full bounded channel (capacity {})", buf.capacity),
                            _span,
                        ));
                    }
                    q.push_back(val);
                    return Some(Value::Unit);
                }
            }
            // B-2026-08-22-21 — the NON-PANICKING send. Where `send` records
            // a runtime error on a full bounded channel (it returns unit, so
            // there is no Err channel), this hands the caller a
            // `Result[(), SendError[T]]` and gives the REJECTED VALUE back
            // inside it — `send` consumes its argument, so a failure that
            // dropped the value would leave no way to retry.
            //
            // `Closed` IS UNREACHABLE ON BOTH BACKENDS, not just this one, and
            // that is a decision rather than an interpreter shortfall. The
            // compiled runtime has the live-end counters to detect a dropped
            // receiver and an earlier draft used them; measured, that made an
            // orphaned sender print `closed` under `karac build` and `sent`
            // here, because `ChannelBuf` is one `Arc` shared by both ends with
            // no per-end liveness. Rather than keep a run-vs-build divergence
            // this project does not accept, the runtime dropped the check —
            // full reasoning in `karac_runtime_channel_try_send`'s doc comment.
            // So both backends agree on `Ok` and on `Full`, which is the case a
            // program can actually provoke.
            "try_send" => {
                let val = args
                    .first()
                    .map(|a| self.eval_expr_inner(&a.value))
                    .unwrap_or(Value::Unit);
                if let Value::Sender(ref buf) = obj {
                    let mut q = buf.queue.lock().unwrap();
                    if buf.capacity != 0 && q.len() >= buf.capacity {
                        drop(q);
                        return Some(Value::EnumVariant {
                            enum_name: "Result".to_string(),
                            variant: "Err".to_string(),
                            data: EnumData::Tuple(vec![Value::EnumVariant {
                                enum_name: "SendError".to_string(),
                                variant: "Full".to_string(),
                                data: EnumData::Tuple(vec![val]),
                            }]),
                        });
                    }
                    q.push_back(val);
                    return Some(Value::EnumVariant {
                        enum_name: "Result".to_string(),
                        variant: "Ok".to_string(),
                        data: EnumData::Tuple(vec![Value::Unit]),
                    });
                }
            }
            // B-2026-08-22-21 — `recv_blocking` shares `recv`'s body. The
            // tree-walk interpreter is single-threaded and cannot park at all,
            // so the two are indistinguishable here by construction; the
            // difference lives in the EFFECT each carries (`blocks` vs
            // `suspends`), which the effect checker seeds separately.
            "recv" | "recv_blocking" => {
                if let Value::Receiver(ref buf) = obj {
                    // In the tree-walk interpreter tests the sender always
                    // fires before recv, so the queue has an item. If empty
                    // (would deadlock in a real runtime) return Unit rather
                    // than blocking the interpreter thread forever.
                    let val = buf.queue.lock().unwrap().pop_front().unwrap_or(Value::Unit);
                    return Some(val);
                }
            }
            "try_recv" => {
                if let Value::Receiver(ref buf) = obj {
                    let opt = buf.queue.lock().unwrap().pop_front();
                    return Some(match opt {
                        Some(v) => Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "Some".to_string(),
                            data: EnumData::Tuple(vec![v]),
                        },
                        None => Value::EnumVariant {
                            enum_name: "Option".to_string(),
                            variant: "None".to_string(),
                            data: EnumData::Unit,
                        },
                    });
                }
            }
            _ => return None,
        }
        None
    }
}
