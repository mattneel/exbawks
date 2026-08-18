//! The gamepad as a USB device: its descriptors and its control requests.
//!
//! A driver enumerates a device by asking it what it is. The answers here
//! describe the Xbox controller, because the title's driver is written
//! against that and will refuse anything else — it looks for the vendor's
//! own interface class rather than for a generic input device.

use crate::gamepad::REPORT_BYTES;

/// The request types a control transfer's setup packet carries.
mod request {
    /// Standard `GET_DESCRIPTOR`.
    pub const GET_DESCRIPTOR: u8 = 6;
    /// Standard `SET_ADDRESS`.
    pub const SET_ADDRESS: u8 = 5;
    /// Standard `SET_CONFIGURATION`.
    pub const SET_CONFIGURATION: u8 = 9;
    /// Standard `GET_CONFIGURATION`.
    pub const GET_CONFIGURATION: u8 = 8;
    /// Standard `GET_STATUS`.
    pub const GET_STATUS: u8 = 0;
    /// Standard `SET_INTERFACE`.
    pub const SET_INTERFACE: u8 = 11;
    /// The class request that reads the controller's state directly, and
    /// the vendor request that asks which of its controls exist.
    pub const GET_REPORT: u8 = 1;
}

/// Descriptor types a driver asks for.
mod descriptor {
    pub const DEVICE: u8 = 1;
    pub const CONFIGURATION: u8 = 2;
    pub const STRING: u8 = 3;
    /// The controller's own descriptor, which says how large its reports
    /// are and what kind of device it is.
    pub const XID: u8 = 0x42;
}

/// The device descriptor: a Microsoft controller with one configuration.
const DEVICE_DESCRIPTOR: [u8; 18] = [
    18,   // bLength
    0x01, // bDescriptorType: device
    0x10, 0x01, // bcdUSB 1.10
    0x00, // bDeviceClass: described by the interface
    0x00, // bDeviceSubClass
    0x00, // bDeviceProtocol
    0x08, // bMaxPacketSize0
    0x5E, 0x04, // idVendor: Microsoft
    0x89, 0x02, // idProduct: controller
    0x00, 0x01, // bcdDevice 1.00
    0x00, // iManufacturer: none
    0x00, // iProduct: none
    0x00, // iSerialNumber: none
    0x01, // bNumConfigurations
];

/// The configuration, its one interface, and the two endpoints a
/// controller carries: reports in, rumble out.
const CONFIGURATION_DESCRIPTOR: [u8; 32] = [
    // Configuration
    9, 0x02, 32, 0x00, 0x01, 0x01, 0x00, 0x80, 50,
    // Interface: the vendor's own class, which is what the driver matches
    9, 0x04, 0x00, 0x00, 0x02, 0x58, 0x42, 0x00, 0x00,
    // Endpoint 1 IN, interrupt, polled every eight frames
    7, 0x05, 0x81, 0x03, 0x20, 0x00, 0x04, // Endpoint 2 OUT, interrupt
    7, 0x05, 0x02, 0x03, 0x20, 0x00, 0x04,
];

/// The controller's own descriptor: a gamepad with a twenty-byte report.
const XID_DESCRIPTOR: [u8; 16] = [
    16,   // bLength
    0x42, // bDescriptorType
    0x00,
    0x01,               // bcdXid
    0x01,               // bType: gamepad
    0x02,               // bSubType: the standard pad
    REPORT_BYTES as u8, // bMaxInputReportSize
    6,                  // bMaxOutputReportSize
    0xFF,
    0xFF,
    0xFF,
    0xFF, // wAlternateProductIds
    0xFF,
    0xFF,
    0xFF,
    0xFF,
];

/// What a control transfer asks for, decoded from its setup packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl Setup {
    /// Decodes the eight bytes a setup stage carries.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 8 {
            return None;
        }
        Some(Self {
            request_type: bytes[0],
            request: bytes[1],
            value: u16::from_le_bytes([bytes[2], bytes[3]]),
            index: u16::from_le_bytes([bytes[4], bytes[5]]),
            length: u16::from_le_bytes([bytes[6], bytes[7]]),
        })
    }

    /// Whether the device answers with data rather than receiving it.
    #[must_use]
    pub fn reads(&self) -> bool {
        self.request_type & 0x80 != 0
    }
}

/// The gamepad's answers to what a driver asks it.
#[derive(Debug, Default, Clone, Copy)]
pub struct GamepadDevice {
    /// The address the driver assigned, once it has.
    address: u8,
    /// The configuration the driver selected.
    configuration: u8,
}

impl GamepadDevice {
    /// The address the driver assigned this device.
    #[must_use]
    pub fn address(&self) -> u8 {
        self.address
    }

    /// Whether the driver has configured the device, after which it polls
    /// the interrupt endpoint for reports.
    #[must_use]
    pub fn configured(&self) -> bool {
        self.configuration != 0
    }

    /// Answers one control transfer.
    ///
    /// Returns the bytes the device sends back, which is empty for a
    /// request that only sets state, or `None` for one it does not
    /// recognise — which a driver reads as a stall.
    pub fn control(&mut self, setup: Setup, report: &[u8; REPORT_BYTES]) -> Option<Vec<u8>> {
        let answer = match (setup.reads(), setup.request) {
            (true, request::GET_DESCRIPTOR) => {
                let kind = (setup.value >> 8) as u8;
                match kind {
                    descriptor::DEVICE => DEVICE_DESCRIPTOR.to_vec(),
                    descriptor::CONFIGURATION => CONFIGURATION_DESCRIPTOR.to_vec(),
                    descriptor::XID => XID_DESCRIPTOR.to_vec(),
                    // No strings are offered, and none are referenced by
                    // the descriptors above, so a driver asking for one
                    // gets a stall rather than an empty answer it would
                    // have to interpret.
                    descriptor::STRING => return None,
                    _ => return None,
                }
            }
            (true, request::GET_CONFIGURATION) => vec![self.configuration],
            (true, request::GET_STATUS) => vec![0, 0],
            // A vendor request asks which controls the pad has, and a
            // class request asks what they are doing. The first wants a
            // mask — every field it supports set to all ones — and
            // answering it with the pad's current state says a pad at
            // rest has no controls at all.
            (true, request::GET_REPORT) if setup.request_type & 0x60 == 0x40 => {
                let mut capabilities = [0xFF_u8; REPORT_BYTES];
                // The two leading bytes stay a report identifier and a
                // length, as they are in a report.
                capabilities[0] = 0x00;
                capabilities[1] = REPORT_BYTES as u8;
                capabilities.to_vec()
            }
            (true, request::GET_REPORT) => report.to_vec(),
            (false, request::SET_ADDRESS) => {
                self.address = (setup.value & 0x7F) as u8;
                Vec::new()
            }
            (false, request::SET_CONFIGURATION) => {
                self.configuration = (setup.value & 0xFF) as u8;
                Vec::new()
            }
            (false, request::SET_INTERFACE) => Vec::new(),
            _ => return None,
        };
        // A driver asks for at most what it has room for.
        let mut answer = answer;
        answer.truncate(setup.length as usize);
        Some(answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(request_type: u8, request: u8, value: u16, length: u16) -> Setup {
        Setup { request_type, request, value, index: 0, length }
    }

    #[test]
    fn a_setup_packet_decodes_its_fields() {
        // GET_DESCRIPTOR(device), 18 bytes wanted.
        let bytes = [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x12, 0x00];
        let parsed = Setup::parse(&bytes).expect("eight bytes is a packet");
        assert_eq!(parsed.request, request::GET_DESCRIPTOR);
        assert_eq!(parsed.value, 0x0100, "descriptor type in the high half");
        assert_eq!(parsed.length, 18);
        assert!(parsed.reads(), "the device answers this one");

        assert!(Setup::parse(&bytes[..7]).is_none(), "a short packet is not one");
    }

    #[test]
    fn the_device_describes_itself_as_a_controller() {
        let mut device = GamepadDevice::default();
        let report = [0_u8; REPORT_BYTES];
        let answer = device
            .control(setup(0x80, request::GET_DESCRIPTOR, 0x0100, 18), &report)
            .expect("a device answers this");

        assert_eq!(answer.len(), 18);
        assert_eq!(u16::from_le_bytes([answer[8], answer[9]]), 0x045E, "Microsoft");
        assert_eq!(answer[17], 1, "one configuration");
    }

    #[test]
    fn the_configuration_names_the_vendor_interface_class() {
        // The driver matches on this class; a generic input device is not
        // what it is looking for and it would ignore one.
        let mut device = GamepadDevice::default();
        let answer = device
            .control(setup(0x80, request::GET_DESCRIPTOR, 0x0200, 32), &[0; REPORT_BYTES])
            .expect("answered");
        assert_eq!(answer[0x0E], 0x58, "interface class");
        assert_eq!(answer[0x0F], 0x42, "interface subclass");
        assert_eq!(answer[0x14], 0x81, "an interrupt endpoint that reads in");
    }

    #[test]
    fn a_short_request_gets_only_what_it_asked_for() {
        // A driver reads the first eight bytes of a descriptor to learn
        // how long it is, and must not be handed more than that.
        let mut device = GamepadDevice::default();
        let answer = device
            .control(setup(0x80, request::GET_DESCRIPTOR, 0x0100, 8), &[0; REPORT_BYTES])
            .expect("answered");
        assert_eq!(answer.len(), 8);
    }

    #[test]
    fn addressing_and_configuring_are_remembered() {
        let mut device = GamepadDevice::default();
        let report = [0_u8; REPORT_BYTES];

        assert_eq!(device.address(), 0);
        assert!(!device.configured());

        assert_eq!(device.control(setup(0x00, request::SET_ADDRESS, 3, 0), &report), Some(vec![]));
        assert_eq!(device.address(), 3);

        assert_eq!(
            device.control(setup(0x00, request::SET_CONFIGURATION, 1, 0), &report),
            Some(vec![])
        );
        assert!(device.configured(), "and now it will be polled for reports");
    }

    #[test]
    fn the_state_can_be_read_through_the_control_endpoint() {
        let mut device = GamepadDevice::default();
        let mut report = [0_u8; REPORT_BYTES];
        report[2] = 0x10;
        let answer = device
            .control(setup(0xA1, request::GET_REPORT, 0x0100, REPORT_BYTES as u16), &report)
            .expect("answered");
        assert_eq!(answer.len(), REPORT_BYTES);
        assert_eq!(answer[2], 0x10, "the buttons reach the driver");
    }

    #[test]
    fn capabilities_report_which_controls_exist_not_their_state() {
        // A pad at rest reports zeroes. Answering the capability request
        // with that state says the pad has no controls, so it has to be
        // answered with a mask instead.
        let mut device = GamepadDevice::default();
        let resting = [0_u8; REPORT_BYTES];

        let capabilities = device
            .control(setup(0xC1, request::GET_REPORT, 0x0100, REPORT_BYTES as u16), &resting)
            .expect("answered");
        assert_eq!(capabilities.len(), REPORT_BYTES);
        assert_eq!(capabilities[1], REPORT_BYTES as u8, "it still states its length");
        assert_eq!(capabilities[2], 0xFF, "every button exists");
        assert_eq!(
            capabilities[4 + crate::gamepad::analogue::LEFT_TRIGGER],
            0xFF,
            "and every trigger"
        );

        // The class request still reports the state, resting or not.
        let state = device
            .control(setup(0xA1, request::GET_REPORT, 0x0100, REPORT_BYTES as u16), &resting)
            .expect("answered");
        assert_eq!(state[2], 0x00, "nothing is pressed");
    }

    #[test]
    fn an_unknown_request_stalls_rather_than_answering_nothing() {
        // An empty answer and a stall are different things to a driver:
        // one is a zero-length reply, the other is a refusal.
        let mut device = GamepadDevice::default();
        assert_eq!(device.control(setup(0x80, 0x7F, 0, 8), &[0; REPORT_BYTES]), None);
        assert_eq!(
            device.control(setup(0x80, request::GET_DESCRIPTOR, 0x0300, 8), &[0; REPORT_BYTES]),
            None,
            "no strings are offered"
        );
    }
}
