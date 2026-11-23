//! The interface between the replica and what it replicates (document 02
//! section 3): execution is a pure function of the state and the committed
//! op.

use granite_base::header::Operation;

/// What a group replicates. The replica calls [`StateMachine::execute`] for
/// each committed op, in op order, on every replica.
///
/// The signature is the whole interface: (op, timestamp, operation, body),
/// and a buffer for the reply. It takes no `Io`, no replica, no view and no
/// clock, so an implementation cannot observe which replica it runs on, in
/// which view, or when; the `tests` module pins the signature.
pub trait StateMachine {
    /// Executes committed op `op`, assigned `timestamp` by its primary,
    /// writes the reply into the start of `reply` and returns its length.
    fn execute(
        &mut self,
        op: u64,
        timestamp: u64,
        operation: Operation,
        body: &[u8],
        reply: &mut [u8],
    ) -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop;

    impl StateMachine for Noop {
        fn execute(&mut self, _: u64, _: u64, _: Operation, _: &[u8], _: &mut [u8]) -> u32 {
            0
        }
    }

    /// The whole signature; nothing else reaches `execute`.
    type Execute = fn(&mut Noop, u64, u64, Operation, &[u8], &mut [u8]) -> u32;

    /// Fails to compile if `execute` gains a parameter, such as the view, the
    /// replica index or a clock.
    #[test]
    fn the_signature_is_op_timestamp_operation_and_body() {
        let execute: Execute = <Noop as StateMachine>::execute;
        assert_eq!(execute(&mut Noop, 1, 2, Operation::None, &[], &mut []), 0);
    }
}
