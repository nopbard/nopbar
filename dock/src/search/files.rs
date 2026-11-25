//! Files from the Windows Search index: its OLE DB provider
//! (Search.CollatorDSO) driven through ADO's IDispatch interface, the way
//! scripts query it. One connection per open panel, on the files worker.

use windows::core::{BSTR, GUID, HSTRING, PCWSTR};
use windows::Win32::System::Com::{
    CLSIDFromProgID, CoCreateInstance, IDispatch, CLSCTX_INPROC_SERVER, DISPATCH_FLAGS, DISPATCH_METHOD, DISPATCH_PROPERTYGET,
    DISPATCH_PROPERTYPUT, DISPPARAMS,
};
use windows::Win32::System::Ole::DISPID_PROPERTYPUT;
use windows::Win32::System::Variant::VARIANT;

pub type Res<T> = std::result::Result<T, String>;

/// Seconds ADO waits to connect, and for a query unless told otherwise
/// (whole seconds only; the panel drops answers that took longer than its
/// own limit, and gives a query more time after "search again").
const TIMEOUT_S: i32 = 2;

unsafe fn invoke(d: &IDispatch, name: &str, flags: DISPATCH_FLAGS, args: &mut [VARIANT]) -> Res<VARIANT> {
    let wide = HSTRING::from(name);
    let names = [PCWSTR(wide.as_ptr())];
    let mut id = 0i32;
    d.GetIDsOfNames(&GUID::zeroed(), names.as_ptr(), 1, 0, &mut id).map_err(|e| format!("{name}: {e}"))?;
    // Arguments go in reverse order.
    args.reverse();
    let mut put = DISPID_PROPERTYPUT;
    let params = DISPPARAMS {
        rgvarg: if args.is_empty() { std::ptr::null_mut() } else { args.as_mut_ptr() },
        rgdispidNamedArgs: if flags == DISPATCH_PROPERTYPUT { &mut put } else { std::ptr::null_mut() },
        cArgs: args.len() as u32,
        cNamedArgs: if flags == DISPATCH_PROPERTYPUT { 1 } else { 0 },
    };
    let mut out = VARIANT::default();
    d.Invoke(id, &GUID::zeroed(), 0, flags, &params, Some(&mut out), None, None).map_err(|e| format!("{name}: {e}"))?;
    Ok(out)
}

unsafe fn get(d: &IDispatch, name: &str, args: &mut [VARIANT]) -> Res<VARIANT> {
    invoke(d, name, DISPATCH_PROPERTYGET, args)
}

unsafe fn object(v: &VARIANT, what: &str) -> Res<IDispatch> {
    IDispatch::try_from(v).map_err(|e| format!("{what}: {e}"))
}

/// '…' for SQL: quotes doubled.
fn sql_text(s: &str) -> String {
    s.replace('\'', "''")
}

/// A LIKE pattern matching `s` anywhere ([%], [_] and [[] stay literal).
fn like_anywhere(s: &str) -> String {
    let mut p = String::from("%");
    for c in s.chars() {
        match c {
            '%' | '_' | '[' => {
                p.push('[');
                p.push(c);
                p.push(']');
            }
            _ => p.push(c),
        }
    }
    p.push('%');
    sql_text(&p)
}

/// The index query for `text`: names containing it, or whose words start
/// with it, best ranked first. Only files on disk (not mail and the like).
pub fn sql(text: &str, top: usize) -> String {
    // CONTAINS takes a quoted phrase: no double quotes inside it.
    let phrase: String = text.chars().filter(|&c| c != '"').collect();
    format!(
        "SELECT TOP {top} System.ItemPathDisplay, System.ItemNameDisplay FROM SystemIndex \
         WHERE SCOPE='file:' AND (System.FileName LIKE '{}' OR CONTAINS(System.FileName, '\"{}*\"')) \
         ORDER BY System.Search.Rank DESC",
        like_anywhere(text),
        sql_text(phrase.trim()),
    )
}

pub struct Index {
    conn: IDispatch,
}

impl Index {
    /// Opens the index (COM must be initialized on this thread).
    pub fn open() -> Res<Self> {
        unsafe {
            let clsid = CLSIDFromProgID(&HSTRING::from("ADODB.Connection")).map_err(|e| format!("ADO: {e}"))?;
            let conn: IDispatch = CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER).map_err(|e| format!("ADO: {e}"))?;
            invoke(&conn, "ConnectionTimeout", DISPATCH_PROPERTYPUT, &mut [VARIANT::from(TIMEOUT_S)])?;
            invoke(&conn, "CommandTimeout", DISPATCH_PROPERTYPUT, &mut [VARIANT::from(TIMEOUT_S)])?;
            invoke(&conn, "Open", DISPATCH_METHOD, &mut [VARIANT::from("Provider=Search.CollatorDSO;Extended Properties='Application=Windows';")])?;
            Ok(Index { conn })
        }
    }

    /// (path, display name) of up to `top` files matching `text`; the
    /// index may take up to `secs` seconds.
    pub fn query(&self, text: &str, top: usize, secs: i32) -> Res<Vec<(String, String)>> {
        unsafe {
            invoke(&self.conn, "CommandTimeout", DISPATCH_PROPERTYPUT, &mut [VARIANT::from(secs.max(1))])?;
            let rs = invoke(&self.conn, "Execute", DISPATCH_METHOD, &mut [VARIANT::from(sql(text, top).as_str())])?;
            let rs = object(&rs, "recordset")?;
            let mut out = Vec::new();
            while out.len() < top {
                let eof = get(&rs, "EOF", &mut [])?;
                if bool::try_from(&eof).unwrap_or(true) {
                    break;
                }
                let fields = object(&get(&rs, "Fields", &mut [])?, "fields")?;
                let col = |i: i32| -> Res<String> {
                    let f = object(&get(&fields, "Item", &mut [VARIANT::from(i)])?, "field")?;
                    let v = get(&f, "Value", &mut [])?;
                    Ok(BSTR::try_from(&v).map(|b| b.to_string()).unwrap_or_default())
                };
                let path = col(0)?;
                let name = col(1)?;
                if !path.is_empty() {
                    out.push((path, name));
                }
                invoke(&rs, "MoveNext", DISPATCH_METHOD, &mut [])?;
            }
            let _ = invoke(&rs, "Close", DISPATCH_METHOD, &mut []);
            Ok(out)
        }
    }
}

impl Drop for Index {
    fn drop(&mut self) {
        unsafe {
            let _ = invoke(&self.conn, "Close", DISPATCH_METHOD, &mut []);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_escaped() {
        let q = sql("it's 100%_[x]\"", 20);
        assert!(q.contains("LIKE '%it''s 100[%][_][[]x]\"%'"), "{q}");
        assert!(q.contains("CONTAINS(System.FileName, '\"it''s 100%_[x]*\"')"), "{q}");
        assert!(q.starts_with("SELECT TOP 20 System.ItemPathDisplay, System.ItemNameDisplay FROM SystemIndex"));
        assert!(q.ends_with("ORDER BY System.Search.Rank DESC"));
    }
}
