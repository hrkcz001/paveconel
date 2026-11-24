use super::*;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
#[derive(Clone, Copy)]
enum Op { Read(u16,u8), Write(u16,u8), FailRead(u16), FailWrite(u16,u8) }
struct Script { ops: RefCell<VecDeque<Op>>, calls: Cell<usize> }
impl Script {
    fn new(ops: Vec<Op>) -> Self { Self { ops: RefCell::new(ops.into()), calls: Cell::new(0) } }
}
impl IoBackend for Script {
    fn read_port(&self,p:u16)->io::Result<u8> {
        self.calls.set(self.calls.get()+1);
        match self.ops.borrow_mut().pop_front().expect("unexpected read") {
            Op::Read(q,v) => { assert_eq!(p,q); Ok(v) }
            Op::FailRead(q) => { assert_eq!(p,q); Err(io::Error::other("injected read failure")) }
            _ => panic!("expected write")
        }
    }
    fn write_port(&self,p:u16,v:u8)->io::Result<()> {
        self.calls.set(self.calls.get()+1);
        match self.ops.borrow_mut().pop_front().expect("unexpected write") {
            Op::Write(q,w) => { assert_eq!((p,v),(q,w)); Ok(()) }
            Op::FailWrite(q,w) => { assert_eq!((p,v),(q,w)); Err(io::Error::other("injected write failure")) }
            _ => panic!("expected read")
        }
    }
    fn name(&self)->&'static str { "script" }
}
fn read_ops()->Vec<Op> { vec![Op::Read(EC_CMD,0),Op::Write(EC_CMD,EC_READ),Op::Read(EC_CMD,0),Op::Write(EC_DATA,0x5a),Op::Read(EC_CMD,0),Op::Read(EC_CMD,EC_OBF),Op::Read(EC_DATA,70)] }
fn write_ops()->Vec<Op> { vec![Op::Read(EC_CMD,0),Op::Write(EC_CMD,EC_WRITE),Op::Read(EC_CMD,0),Op::Write(EC_DATA,0x2c),Op::Read(EC_CMD,0),Op::Write(EC_DATA,100),Op::Read(EC_CMD,0)] }
fn assert_latched(ec:&Ec, io:&Script) {
    let n=io.calls.get();
    assert!(ec.read(0x5a).is_err());
    assert!(ec.write(0x2c,100).is_err());
    assert!(ec.read_all().is_err());
    assert!(ec.read_word(0xb0).is_err());
    assert_eq!(n,io.calls.get(),"faulted session performed more I/O");
}
#[test]
fn every_read_and_write_io_failure_stops_at_exact_operation() {
    for write in [false,true] {
        let ops=if write { write_ops() } else { read_ops() };
        for i in 0..ops.len() {
            let mut failed=ops[..=i].to_vec();
            failed[i]=match ops[i] { Op::Read(p,_)=>Op::FailRead(p), Op::Write(p,v)=>Op::FailWrite(p,v), _=>unreachable!() };
            let io=Script::new(failed);
            let ec=Ec::new(&io);
            if write { assert!(ec.write(0x2c,100).is_err()); } else { assert!(ec.read(0x5a).is_err()); }
            assert_eq!(io.calls.get(),i+1);
            assert_latched(&ec,&io);
        }
    }
}
#[test]
fn every_handshake_timeout_stops_without_retry() {
    for write in [false,true] {
        let ops=if write { write_ops() } else { read_ops() };
        let waits=if write { vec![0,2,4,6] } else { vec![0,2,4,5] };
        for i in waits {
            let mut failed=ops[..i].to_vec();
            failed.extend(std::iter::repeat_n(Op::Read(EC_CMD,if !write && i==5 { 0 } else { EC_IBF }),POLL_LIMIT));
            let io=Script::new(failed);
            let ec=Ec::new(&io);
            let e=if write { ec.write(0x2c,100).unwrap_err() } else { ec.read(0x5a).unwrap_err() };
            assert_eq!(e.kind(),io::ErrorKind::TimedOut);
            assert_eq!(io.calls.get(),i+POLL_LIMIT);
            assert_latched(&ec,&io);
        }
    }
}
#[test]
fn successful_transactions_use_expected_byte_order() {
    let io=Script::new(read_ops()); assert_eq!(Ec::new(&io).read(0x5a).unwrap(),70);
    let io=Script::new(write_ops()); Ec::new(&io).write(0x2c,100).unwrap();
}
#[test]
fn driver_read_must_return_exactly_one_byte() {
    for n in [0,2,u32::MAX] { assert!(validate_read_length(n).is_err()); }
    assert!(validate_read_length(1).is_ok());
}
