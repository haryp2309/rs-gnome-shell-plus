//! A minimal libpam binding: authenticates one user with the
//! `gnome-shell-plus` service (data/pam/gnome-shell-plus) on a worker
//! thread. The conversation is relayed to the caller, so PAM modules can
//! ask for anything (password, OTP code…) and report messages.
//!
//! As an unprivileged process, pam_unix checks the user's own password
//! through its setuid `unix_chkpwd` helper.
//!
//! libpam is linked by soname: distributions without pam-devel have no
//! `libpam.so` symlink.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;
use std::sync::mpsc;

use gtk::glib;

pub const SERVICE: &str = "gnome-shell-plus";

const PAM_SUCCESS: c_int = 0;
const PAM_BUF_ERR: c_int = 5;
const PAM_CONV_ERR: c_int = 19;
const PAM_NEW_AUTHTOK_REQD: c_int = 12;
const PAM_REINITIALIZE_CRED: c_int = 0x0008;

const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

type ConvFn = unsafe extern "C" fn(c_int, *mut *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int;

#[repr(C)]
struct PamConv {
    conv: ConvFn,
    appdata_ptr: *mut c_void,
}

#[repr(C)]
struct PamHandle {
    _private: [u8; 0],
}

#[link(name = "libpam.so.0", kind = "dylib", modifiers = "+verbatim")]
extern "C" {
    fn pam_start(service: *const c_char, user: *const c_char, conv: *const PamConv, handle: *mut *mut PamHandle) -> c_int;
    fn pam_authenticate(handle: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_acct_mgmt(handle: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_setcred(handle: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_end(handle: *mut PamHandle, status: c_int) -> c_int;
    fn pam_strerror(handle: *mut PamHandle, errnum: c_int) -> *const c_char;
}

extern "C" {
    fn calloc(n: usize, size: usize) -> *mut c_void;
    fn free(p: *mut c_void);
    fn strdup(s: *const c_char) -> *mut c_char;
}

/// What the PAM thread reports.
#[derive(Debug)]
pub enum Message {
    Info(String),
    Problem(String),
    /// Waits for an answer through the reply channel.
    Query { prompt: String, secret: bool },
    Done(Result<(), String>),
}

struct Conversation {
    send: Box<dyn Fn(Message) + Send>,
    answers: mpsc::Receiver<String>,
}

/// Runs a PAM transaction for `user` on a new thread. `send` gets every
/// message (on the PAM thread); answers to queries go into the returned
/// sender. Dropping the sender cancels the conversation.
pub fn authenticate(user: &str, send: impl Fn(Message) + Send + 'static) -> mpsc::Sender<String> {
    let (answer_tx, answer_rx) = mpsc::channel();
    let user = user.to_owned();
    let spawned = std::thread::Builder::new().name("pam".into()).spawn(move || {
        let conversation = Conversation {
            send: Box::new(send),
            answers: answer_rx,
        };
        let result = run(&user, &conversation);
        (conversation.send)(Message::Done(result));
    });
    if let Err(err) = spawned {
        glib::g_warning!("shellplus", "Can't start PAM thread: {err}");
    }
    answer_tx
}

fn run(user: &str, conversation: &Conversation) -> Result<(), String> {
    let service = CString::new(SERVICE).unwrap();
    let user = CString::new(user).map_err(|_| "bad user name".to_owned())?;
    let conv = PamConv {
        conv: converse,
        appdata_ptr: conversation as *const Conversation as *mut c_void,
    };
    let mut handle: *mut PamHandle = ptr::null_mut();

    // SAFETY: all pointers outlive the transaction, which ends in pam_end
    // before `conv` and `conversation` go out of scope.
    unsafe {
        let mut status = pam_start(service.as_ptr(), user.as_ptr(), &conv, &mut handle);
        if status != PAM_SUCCESS {
            return Err(format!("pam_start failed ({status})"));
        }
        status = pam_authenticate(handle, 0);
        if status == PAM_SUCCESS {
            status = pam_acct_mgmt(handle, 0);
            // An expired password was still the right one; the lock screen
            // can't change it, so let the user in (GDM would ask for a new one).
            if status == PAM_NEW_AUTHTOK_REQD {
                status = PAM_SUCCESS;
            }
        }
        if status == PAM_SUCCESS {
            // Refresh Kerberos tickets and the like; failure doesn't matter.
            pam_setcred(handle, PAM_REINITIALIZE_CRED);
        }
        let error = (status != PAM_SUCCESS).then(|| {
            let text = pam_strerror(handle, status);
            if text.is_null() {
                format!("PAM error {status}")
            } else {
                CStr::from_ptr(text).to_string_lossy().into_owned()
            }
        });
        pam_end(handle, status);
        match error {
            None => Ok(()),
            Some(error) => Err(error),
        }
    }
}

unsafe extern "C" fn converse(
    n: c_int,
    messages: *mut *const PamMessage,
    responses: *mut *mut PamResponse,
    data: *mut c_void,
) -> c_int {
    if n <= 0 || messages.is_null() || responses.is_null() || data.is_null() {
        return PAM_CONV_ERR;
    }
    let conversation = &*(data as *const Conversation);
    let replies = calloc(n as usize, std::mem::size_of::<PamResponse>()) as *mut PamResponse;
    if replies.is_null() {
        return PAM_BUF_ERR;
    }

    for i in 0..n as usize {
        // Linux-PAM passes an array of pointers to messages.
        let message = &**messages.add(i);
        let text = if message.msg.is_null() {
            String::new()
        } else {
            CStr::from_ptr(message.msg).to_string_lossy().into_owned()
        };
        match message.msg_style {
            PAM_PROMPT_ECHO_OFF | PAM_PROMPT_ECHO_ON => {
                (conversation.send)(Message::Query {
                    prompt: text,
                    secret: message.msg_style == PAM_PROMPT_ECHO_OFF,
                });
                let Ok(answer) = conversation.answers.recv() else {
                    free_replies(replies, i);
                    return PAM_CONV_ERR;
                };
                // Don't leave the secret lying around in our copies.
                let copy = match CString::new(answer.into_bytes()) {
                    Ok(c) => {
                        let p = strdup(c.as_ptr());
                        c.into_bytes_with_nul().iter_mut().for_each(|b| *b = 0);
                        Some(p)
                    }
                    Err(err) => {
                        err.into_vec().iter_mut().for_each(|b| *b = 0);
                        None
                    }
                };
                match copy {
                    Some(p) if !p.is_null() => (*replies.add(i)).resp = p,
                    _ => {
                        free_replies(replies, i);
                        return PAM_CONV_ERR;
                    }
                }
            }
            PAM_ERROR_MSG => (conversation.send)(Message::Problem(text)),
            PAM_TEXT_INFO => (conversation.send)(Message::Info(text)),
            _ => {
                free_replies(replies, i);
                return PAM_CONV_ERR;
            }
        }
    }
    *responses = replies;
    PAM_SUCCESS
}

/// Frees the first `filled` responses' strings (wiped) and the array.
unsafe fn free_replies(replies: *mut PamResponse, filled: usize) {
    for i in 0..filled {
        let resp = (*replies.add(i)).resp;
        if !resp.is_null() {
            let len = CStr::from_ptr(resp).to_bytes().len();
            ptr::write_bytes(resp, 0, len);
            free(resp as *mut c_void);
        }
    }
    free(replies as *mut c_void);
}
