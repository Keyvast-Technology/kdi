//! Open a real board over USB3 and read its handshake — the first ten minutes on Windows:
//! `usb3_handshake [serial] [driver-dir]`. Identity registers BY NAME, no endpoint number here.
//! Does NOT flash: `open_usb3` binds what is running, safe on a shared board (`bundled` covers it).

use kdi::Commands;

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let serial = args.next().unwrap_or_default();
    let driver_dir = args.next();

    let opened = kdi::Device::open_usb3(&serial, driver_dir.as_deref().map(std::path::Path::new));

    let mut dev = match opened {
        Ok(d) => d,
        Err(e) => {
            // Naming the failure matters more than the exit code: "no board" and "the driver is
            // present but a symbol did not resolve" are the two outcomes worth telling apart, and
            // every entry point resolves by name at dlopen so the second one says which symbol.
            eprintln!("could not open the board: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let (major, minor) = dev.kdi();
    let caps = dev.caps();
    println!("serial          {}", dev.info().serial);
    println!("transport       {:?}", dev.info().transport);
    println!("contract        {major}.{minor}");
    println!("caps            0x{:08x}", caps.0);
    for c in caps.iter() {
        println!("                  {}", c.token());
    }
    println!("gateware_sha    {:08x}", dev.gateware_sha());
    if caps.has(kdi::Cap::CommandProtocol) {
        match dev.sys_hello() {
            Ok(h) => println!("fw_sha          {}", h.fw),
            Err(e) => {
                eprintln!("sys.hello failed: {e}");
                return std::process::ExitCode::FAILURE;
            }
        }
    } else {
        println!("fw_sha          -");
    }

    match dev.wait_ready(std::time::Duration::from_millis(kdi::READY_TIMEOUT_MS)) {
        Ok(()) => println!("contract_ready  yes"),
        Err(e) => {
            // Not fatal for a handshake read: a board still calibrating is a normal transient, and
            // the point of this program is to report what the device says.
            println!("contract_ready  NO ({e})");
        }
    }

    if let Err(e) = dev.close() {
        eprintln!("close: {e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}
