use super::*;
use std::cell::RefCell;
struct Ports { writes: RefCell<Vec<(u16,u8)>> }
impl IoBackend for Ports {
    fn read_port(&self, _: u16) -> io::Result<u8> { Ok(1) }
    fn write_port(&self, p: u16, v: u8) -> io::Result<()> { self.writes.borrow_mut().push((p,v)); Ok(()) }
    fn name(&self) -> &'static str { "fake" }
}
#[test]
fn garbage_telemetry_forces_maximum_not_a_lower_curve_duty() {
    for (cpu,gpu,duty,rpm) in [(0,40,40,3000), (255,40,40,3000), (50,255,40,3000), (50,40,255,3000), (50,40,40,65535)] {
        let io = Ports { writes: RefCell::new(Vec::new()) };
        let ec = Ec::new(&io);
        let t = Telemetry { cpu_temp: cpu, gpu_temp: gpu, cpu_duty: duty, cpu_rpm: rpm, ..Default::default() };
        assert_eq!(step(&ec, &Config::default(), &t, true).unwrap(), (100.0,100.0));
    }
}
#[test]
fn gpu_asleep_is_not_bad_telemetry() {
    let io = Ports { writes: RefCell::new(Vec::new()) };
    let ec = Ec::new(&io);
    let t = Telemetry { cpu_temp: 60, gpu_temp: 0, ..Default::default() };
    assert!(step(&ec, &Config::default(), &t, true).unwrap().0 < 100.0);
}
#[test]
fn explicit_exit_policy_runs_once() {
    let io = Ports { writes: RefCell::new(Vec::new()) };
    let ec = Ec::new(&io);
    { let guard = ExitGuard::new(&ec, ExitPolicy::MaxFans); guard.apply().unwrap(); }
    assert_eq!(io.writes.borrow().len(), 9);
}
#[test]
fn faulted_transport_has_no_exit_writes() {
    struct Broken { calls: Cell<usize> }
    impl IoBackend for Broken {
        fn read_port(&self, _: u16) -> io::Result<u8> { self.calls.set(self.calls.get()+1); Err(io::Error::other("injected")) }
        fn write_port(&self, _: u16, _: u8) -> io::Result<()> { panic!("must not write after failure") }
        fn name(&self) -> &'static str { "broken" }
    }
    let io = Broken { calls: Cell::new(0) };
    let ec = Ec::new(&io);
    { let _guard = ExitGuard::new(&ec, ExitPolicy::MaxFans); assert!(ec.read(CPU_TEMP).is_err()); }
    assert_eq!(io.calls.get(),1);
    let io = Broken { calls: Cell::new(0) };
    let shared = Shared::new("25".into(), "25".into(), true);
    run(&io, &Config::default(), &shared, false);
    assert_eq!(io.calls.get(),1, "loop must not retry failed telemetry or write an exit recipe");
    assert!(shared.error().contains("transport fault"));
}
