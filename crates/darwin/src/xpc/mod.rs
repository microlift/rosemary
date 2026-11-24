//! libxpc, which Apple has never released, over this crate's Mach ports.
//!
//! The objects are in [`object`] and [`collection`], the wire format in
//! [`codec`] and connections in [`connection`].

pub mod codec;
pub mod collection;
pub mod connection;
pub mod object;

use std::ffi::{CStr, c_char};

use collection::*;
use connection::*;
use object::*;

/// A `const char *const` data symbol, which a guest reads through its slot.
#[repr(transparent)]
struct Key(*const c_char);

// SAFETY: a pointer to a `'static` C string literal, never written.
unsafe impl Sync for Key {}

impl Key {
    const fn of(text: &'static CStr) -> Key {
        Key(text.as_ptr())
    }

    fn address(&'static self) -> u64 {
        self as *const Key as u64
    }
}

static ERROR_KEY_DESCRIPTION: Key = Key::of(connection::KEY_DESCRIPTION);
/// `XPC_EVENT_KEY_NAME`, which launchd's event streams set.
static EVENT_KEY_NAME: Key = Key::of(c"XPCEventName");

exports::exports! {
    functions {
        "xpc_retain" => xpc_retain,
        "xpc_release" => xpc_release,
        "xpc_get_type" => xpc_get_type,
        "xpc_type_get_name" => xpc_type_get_name,
        "xpc_copy" => xpc_copy,
        "xpc_equal" => xpc_equal,
        "xpc_hash" => xpc_hash,
        "xpc_copy_description" => xpc_copy_description,

        "xpc_null_create" => xpc_null_create,
        "xpc_bool_create" => xpc_bool_create,
        "xpc_bool_get_value" => xpc_bool_get_value,
        "xpc_int64_create" => xpc_int64_create,
        "xpc_int64_get_value" => xpc_int64_get_value,
        "xpc_uint64_create" => xpc_uint64_create,
        "xpc_uint64_get_value" => xpc_uint64_get_value,
        "xpc_double_create" => xpc_double_create,
        "xpc_double_get_value" => xpc_double_get_value,
        "xpc_date_create" => xpc_date_create,
        "xpc_date_create_from_current" => xpc_date_create_from_current,
        "xpc_date_get_value" => xpc_date_get_value,
        "xpc_data_create" => xpc_data_create,
        "xpc_data_get_bytes_ptr" => xpc_data_get_bytes_ptr,
        "xpc_data_get_length" => xpc_data_get_length,
        "xpc_data_get_bytes" => xpc_data_get_bytes,
        "xpc_string_create" => xpc_string_create,
        "xpc_string_get_string_ptr" => xpc_string_get_string_ptr,
        "xpc_string_get_length" => xpc_string_get_length,
        "xpc_uuid_create" => xpc_uuid_create,
        "xpc_uuid_get_bytes" => xpc_uuid_get_bytes,

        "xpc_dictionary_create" => xpc_dictionary_create,
        "xpc_dictionary_create_empty" => xpc_dictionary_create_empty,
        "xpc_dictionary_create_reply" => xpc_dictionary_create_reply,
        "xpc_dictionary_get_remote_connection" => xpc_dictionary_get_remote_connection,
        "xpc_dictionary_set_value" => xpc_dictionary_set_value,
        "xpc_dictionary_get_value" => xpc_dictionary_get_value,
        "xpc_dictionary_get_count" => xpc_dictionary_get_count,
        "xpc_dictionary_apply" => xpc_dictionary_apply,
        "xpc_dictionary_set_bool" => xpc_dictionary_set_bool,
        "xpc_dictionary_get_bool" => xpc_dictionary_get_bool,
        "xpc_dictionary_set_int64" => xpc_dictionary_set_int64,
        "xpc_dictionary_get_int64" => xpc_dictionary_get_int64,
        "xpc_dictionary_set_uint64" => xpc_dictionary_set_uint64,
        "xpc_dictionary_get_uint64" => xpc_dictionary_get_uint64,
        "xpc_dictionary_set_double" => xpc_dictionary_set_double,
        "xpc_dictionary_get_double" => xpc_dictionary_get_double,
        "xpc_dictionary_set_date" => xpc_dictionary_set_date,
        "xpc_dictionary_get_date" => xpc_dictionary_get_date,
        "xpc_dictionary_set_string" => xpc_dictionary_set_string,
        "xpc_dictionary_get_string" => xpc_dictionary_get_string,
        "xpc_dictionary_set_data" => xpc_dictionary_set_data,
        "xpc_dictionary_get_data" => xpc_dictionary_get_data,
        "xpc_dictionary_set_uuid" => xpc_dictionary_set_uuid,
        "xpc_dictionary_get_uuid" => xpc_dictionary_get_uuid,
        "xpc_dictionary_get_dictionary" => xpc_dictionary_get_dictionary,
        "xpc_dictionary_get_array" => xpc_dictionary_get_array,

        "xpc_array_create" => xpc_array_create,
        "xpc_array_create_empty" => xpc_array_create_empty,
        "xpc_array_append_value" => xpc_array_append_value,
        "xpc_array_set_value" => xpc_array_set_value,
        "xpc_array_get_value" => xpc_array_get_value,
        "xpc_array_get_count" => xpc_array_get_count,
        "xpc_array_apply" => xpc_array_apply,
        "xpc_array_set_bool" => xpc_array_set_bool,
        "xpc_array_get_bool" => xpc_array_get_bool,
        "xpc_array_set_int64" => xpc_array_set_int64,
        "xpc_array_get_int64" => xpc_array_get_int64,
        "xpc_array_set_uint64" => xpc_array_set_uint64,
        "xpc_array_get_uint64" => xpc_array_get_uint64,
        "xpc_array_set_double" => xpc_array_set_double,
        "xpc_array_get_double" => xpc_array_get_double,
        "xpc_array_set_date" => xpc_array_set_date,
        "xpc_array_get_date" => xpc_array_get_date,
        "xpc_array_set_string" => xpc_array_set_string,
        "xpc_array_get_string" => xpc_array_get_string,
        "xpc_array_set_data" => xpc_array_set_data,
        "xpc_array_get_data" => xpc_array_get_data,
        "xpc_array_get_dictionary" => xpc_array_get_dictionary,
        "xpc_array_get_array" => xpc_array_get_array,

        "xpc_connection_create" => xpc_connection_create,
        "xpc_connection_create_mach_service" => xpc_connection_create_mach_service,
        "xpc_connection_create_from_endpoint" => xpc_connection_create_from_endpoint,
        "xpc_endpoint_create" => xpc_endpoint_create,
        "xpc_connection_set_event_handler" => xpc_connection_set_event_handler,
        "xpc_connection_set_target_queue" => xpc_connection_set_target_queue,
        "xpc_connection_resume" => xpc_connection_resume,
        "xpc_connection_activate" => xpc_connection_activate,
        "xpc_connection_suspend" => xpc_connection_suspend,
        "xpc_connection_cancel" => xpc_connection_cancel,
        "xpc_connection_send_message" => xpc_connection_send_message,
        "xpc_connection_send_message_with_reply" => xpc_connection_send_message_with_reply,
        "xpc_connection_send_message_with_reply_sync" => xpc_connection_send_message_with_reply_sync,
        "xpc_connection_send_barrier" => xpc_connection_send_barrier,
        "xpc_connection_get_name" => xpc_connection_get_name,
        "xpc_connection_get_pid" => xpc_connection_get_pid,
        "xpc_connection_get_euid" => xpc_connection_get_euid,
        "xpc_connection_get_egid" => xpc_connection_get_egid,
        "xpc_connection_get_asid" => xpc_connection_get_asid,
        "xpc_connection_set_context" => xpc_connection_set_context,
        "xpc_connection_get_context" => xpc_connection_get_context,
        "xpc_connection_set_finalizer_f" => xpc_connection_set_finalizer_f,
        "xpc_connection_copy_entitlement_value" => xpc_connection_copy_entitlement_value,
    }
    // `XPC_TYPE_*`, `XPC_BOOL_*` and `XPC_ERROR_*` are the addresses of these
    // symbols, compared by the guest without a call.
    data {
        "__xpc_type_connection" => Type::Connection.token(),
        "__xpc_type_endpoint" => Type::Endpoint.token(),
        "__xpc_type_null" => Type::Null.token(),
        "__xpc_type_bool" => Type::Bool.token(),
        "__xpc_type_int64" => Type::Int64.token(),
        "__xpc_type_uint64" => Type::Uint64.token(),
        "__xpc_type_double" => Type::Double.token(),
        "__xpc_type_date" => Type::Date.token(),
        "__xpc_type_data" => Type::Data.token(),
        "__xpc_type_string" => Type::String.token(),
        "__xpc_type_uuid" => Type::Uuid.token(),
        "__xpc_type_fd" => Type::Fd.token(),
        "__xpc_type_shmem" => Type::Shmem.token(),
        "__xpc_type_array" => Type::Array.token(),
        "__xpc_type_dictionary" => Type::Dictionary.token(),
        "__xpc_type_error" => Type::Error.token(),
        "__xpc_bool_true" => bool_object(true),
        "__xpc_bool_false" => bool_object(false),
        "__xpc_error_connection_interrupted" => error_interrupted(),
        "__xpc_error_connection_invalid" => error_invalid(),
        "__xpc_error_termination_imminent" => error_termination_imminent(),
        "__xpc_error_key_description" => ERROR_KEY_DESCRIPTION.address(),
        "__xpc_event_key_name" => EVENT_KEY_NAME.address(),
    }
}
