use std::any::Any;

use super::panic_payload_text;

#[test]
fn panic_payload_extracts_message() {
    let payload: Box<dyn Any + Send> = Box::new("boom");
    assert_eq!(panic_payload_text(&*payload), "boom");

    let payload: Box<dyn Any + Send> = Box::new("owned boom".to_string());
    assert_eq!(panic_payload_text(&*payload), "owned boom");

    let payload: Box<dyn Any + Send> = Box::new(42_i32);
    assert_eq!(panic_payload_text(&*payload), "未知负载");
}
