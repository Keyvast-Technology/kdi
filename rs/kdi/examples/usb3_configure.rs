//! Load a caller-supplied bitstream onto a board, bind it, print its identity: `usb3_configure
//! <bit> [serial]`, `<bit>` being **the** release bitstream (`make all`). Volatile — no flash — but
//! it evicts whatever was running: NOT safe on a shared board. Compare the printed sha yourself.

use std::fs;
use std::process::ExitCode;

use kdi::Commands;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(bit) = args.next() else {
        eprintln!("usage: usb3_configure <bit> [serial]");
        return ExitCode::FAILURE;
    };
    let serial = args.next().unwrap_or_default();

    let image = match fs::read(&bit) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{bit}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut dev = match kdi::Device::open_usb3_configured(&serial, &image) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("configure + open failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let (major, minor) = dev.kdi();
    let sha = dev.gateware_sha();
    let caps = dev.caps();
    println!("serial          {}", dev.info().serial);
    println!("contract        {major}.{minor}");
    println!("gateware_sha    {sha:08x}");
    println!("caps            0x{:08x}", caps.0);
    if caps.has(kdi::Cap::CommandProtocol) {
        match dev.sys_hello() {
            Ok(h) => println!("fw_sha          {}", h.fw),
            Err(e) => {
                eprintln!("sys.hello failed: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        println!("fw_sha          -");
    }

    if let Err(e) = dev.close() {
        eprintln!("close: {e}");
        return ExitCode::FAILURE;
    }
    println!("configure OK");
    ExitCode::SUCCESS
}
