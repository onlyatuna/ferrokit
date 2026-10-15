// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_platform::{SingleInstance, SingleInstanceCommand, SingleInstanceResult};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[test]
fn test_single_instance_command_conversion() {
    assert_eq!(
        SingleInstanceCommand::from_u8(1),
        Some(SingleInstanceCommand::WakeUp)
    );
    assert_eq!(
        SingleInstanceCommand::from_u8(2),
        Some(SingleInstanceCommand::Toggle)
    );
    assert_eq!(
        SingleInstanceCommand::from_u8(3),
        Some(SingleInstanceCommand::Show)
    );
    assert_eq!(
        SingleInstanceCommand::from_u8(4),
        Some(SingleInstanceCommand::Quit)
    );
    assert_eq!(SingleInstanceCommand::from_u8(99), None);
}

#[test]
fn test_single_instance_guard_callbacks() {
    let triggered = Arc::new(AtomicBool::new(false));
    let triggered_clone = Arc::clone(&triggered);

    let res = SingleInstance::acquire(
        "FerrokitTestCallbackApp",
        SingleInstanceCommand::WakeUp,
    );

    match &res {
        SingleInstanceResult::Primary(guard) => {
            guard.on_command(move |cmd| {
                if cmd == SingleInstanceCommand::Toggle {
                    triggered_clone.store(true, Ordering::SeqCst);
                }
            });

            guard.dispatch_command(SingleInstanceCommand::Toggle);
            assert!(
                triggered.load(Ordering::SeqCst),
                "Command callback should be invoked upon dispatch"
            );
        }
        SingleInstanceResult::Secondary { .. } => {
            panic!("Expected primary instance in callback test");
        }
    }
}

#[test]
fn test_single_instance_primary_and_secondary_conflict() {
    let app_name = "FerrokitTestUniqueAppConflictTest";

    // 1. First instance acquires Primary
    let primary_res = SingleInstance::acquire(app_name, SingleInstanceCommand::WakeUp);
    let primary_guard = match primary_res {
        SingleInstanceResult::Primary(guard) => guard,
        SingleInstanceResult::Secondary { .. } => {
            panic!("First acquire must succeed as Primary");
        }
    };

    // 2. Second instance attempts acquire while primary is alive -> returns Secondary
    let secondary_res = SingleInstance::acquire(app_name, SingleInstanceCommand::WakeUp);
    match &secondary_res {
        SingleInstanceResult::Secondary { command_sent } => {
            assert!(
                *command_sent,
                "Secondary instance should successfully send wake-up command"
            );
        }
        SingleInstanceResult::Primary(_) => {
            panic!("Concurrent acquire while primary is alive must return Secondary");
        }
    }

    // 3. Drop primary guard to release lock
    drop(primary_guard);

    // Give OS a tiny window to close mutex / lock file
    std::thread::sleep(std::time::Duration::from_millis(50));

    // 4. Third instance acquires lock -> now becomes Primary!
    let third_res = SingleInstance::acquire(app_name, SingleInstanceCommand::WakeUp);
    match &third_res {
        SingleInstanceResult::Primary(_) => {
            // Succeeded in becoming primary after prior guard was dropped
        }
        SingleInstanceResult::Secondary { .. } => {
            panic!("Acquire after dropping primary guard must become Primary");
        }
    }
}
