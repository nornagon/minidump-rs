use std::{io::{BufWriter, Write}, path::PathBuf, time::Duration, ops::Deref};

use minidump::*;
use minidump_processor::{
    http_symbol_supplier, simple_symbol_supplier, MultiSymbolProvider, Symbolizer,
};
use neon::prelude::*;
use once_cell::sync::OnceCell;
use tokio::runtime::Runtime;

// Return a global tokio runtime or create one if it doesn't exist.
// Throws a JavaScript exception if the `Runtime` fails to create.
fn runtime<'a, C: Context<'a>>(cx: &mut C) -> NeonResult<&'static Runtime> {
    static RUNTIME: OnceCell<Runtime> = OnceCell::new();

    RUNTIME.get_or_try_init(|| Runtime::new().or_else(|err| cx.throw_error(err.to_string())))
}

fn minidump_stackwalk(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let rt = runtime(&mut cx)?;
    let channel = cx.channel();

    let minidump_path: Handle<JsString> = cx.argument(0)?;
    let minidump_path = PathBuf::from(minidump_path.value(&mut cx));
    let opts: Option<Handle<JsValue>> = cx.argument_opt(1);
    let opts = match opts {
        Some(o) => o.downcast_or_throw::<JsObject, FunctionContext>(&mut cx)?,
        None => cx.empty_object(),
    };
    let symbol_urls: Option<Handle<JsArray>> = opts.get_opt(&mut cx, "symbolUrls")?;
    let symbol_urls = match symbol_urls {
        Some(arr) => arr.to_vec(&mut cx)?,
        None => vec![],
    };
    let mut symbol_urls_strs = Vec::<String>::new();
    for symbol_url in symbol_urls.iter() {
        let str: Handle<JsString> = symbol_url.downcast_or_throw(&mut cx)?;
        symbol_urls_strs.push(str.value(&mut cx));
    }

    let symbol_paths: Option<Handle<JsArray>> = opts.get_opt(&mut cx, "symbolPaths")?;
    let symbol_paths = match symbol_paths {
        Some(arr) => arr.to_vec(&mut cx)?,
        None => vec![],
    };
    let mut symbol_paths_strs = Vec::<PathBuf>::new();
    for symbol_path in symbol_paths.iter() {
        let str: Handle<JsString> = symbol_path.downcast_or_throw(&mut cx)?;
        symbol_paths_strs.push(PathBuf::from(str.value(&mut cx)));
    }

    let temp_dir = std::env::temp_dir();

    let symbols_cache: Option<Handle<JsString>> = opts.get_opt(&mut cx, "symbolsCache")?;
    let symbols_cache = symbols_cache.map(|x| PathBuf::from(x.value(&mut cx)));
    let symbols_cache = symbols_cache.unwrap_or_else(|| temp_dir.join("rust-minidump-cache"));

    let symbols_tmp: Option<Handle<JsString>> = opts.get_opt(&mut cx, "symbolsTemp")?;
    let symbols_tmp = symbols_tmp.map(|x| PathBuf::from(x.value(&mut cx)));
    let symbols_tmp = symbols_tmp.unwrap_or(temp_dir);

    let timeout: Option<Handle<JsNumber>> = opts.get_opt(&mut cx, "timeout")?;
    let timeout = timeout.map(|x| x.value(&mut cx));

    let timeout = Duration::from_secs_f64(timeout.unwrap_or(1000.0));

    // Create a JavaScript promise and a `deferred` handle for resolving it.
    // It is important to be careful not to perform failable actions after
    // creating the promise to avoid an unhandled rejection.
    let (deferred, promise) = cx.promise();

    rt.spawn(async move {
        match Minidump::read_path(minidump_path) {
            Ok(dump) => {
                let mut provider = MultiSymbolProvider::new();
                if !symbol_urls_strs.is_empty() {
                    provider.add(Box::new(Symbolizer::new(http_symbol_supplier(
                        symbol_paths_strs,
                        symbol_urls_strs,
                        symbols_cache,
                        symbols_tmp,
                        timeout,
                    ))));
                } else if !symbol_paths_strs.is_empty() {
                    provider.add(Box::new(Symbolizer::new(simple_symbol_supplier(
                        symbol_paths_strs,
                    ))));
                }

                let res = minidump_processor::process_minidump(&dump, &provider).await;
                deferred.settle_with(&channel, move |mut cx| {
                    match res {
                        Ok(state) => {
                            let mut buf = BufWriter::new(Vec::new());

                            state.print(&mut buf).unwrap();
                            
                            // TODO: optionally return JSON?
                            //state.print_json(&mut buf, false).unwrap();

                            let bytes = buf.into_inner().unwrap();
                            let string = String::from_utf8(bytes).unwrap();
                            Ok(cx.string(string))
                        }
                        Err(err) => cx.throw_error(format!(
                            "{} - Error processing dump: {}",
                            err.name(),
                            err
                        )),
                    }
                })
            }
            Err(err) => deferred.settle_with(&channel, move |mut cx| {
                let x: NeonResult<Handle<JsValue>> =
                    cx.throw_error(format!("{} - Error reading dump: {}", err.name(), err));
                x
            }),
        };
    });

    Ok(promise)
}

fn minidump_dump(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let rt = runtime(&mut cx)?;
    let channel = cx.channel();

    let minidump_path: Handle<JsString> = cx.argument(0)?;
    let minidump_path = PathBuf::from(minidump_path.value(&mut cx));
    let opts: Option<Handle<JsValue>> = cx.argument_opt(1);
    let opts = match opts {
        Some(o) => o.downcast_or_throw::<JsObject, FunctionContext>(&mut cx)?,
        None => cx.empty_object(),
    };
    
    let brief: Option<Handle<JsBoolean>> = opts.get_opt(&mut cx, "brief")?;
    let brief = brief.map(|x| x.value(&mut cx)).unwrap_or(false);

    // Create a JavaScript promise and a `deferred` handle for resolving it.
    // It is important to be careful not to perform failable actions after
    // creating the promise to avoid an unhandled rejection.
    let (deferred, promise) = cx.promise();

    rt.spawn(async move {
        match Minidump::read_path(minidump_path) {
            Ok(dump) => {
                deferred.settle_with(&channel, move |mut cx| {
                    let mut buf = BufWriter::new(Vec::new());
                    match print_minidump_dump(&dump, &mut buf, brief) {
                        Ok(_) => {
                            let bytes = buf.into_inner().unwrap();
                            let string = String::from_utf8(bytes).unwrap();
                            Ok(cx.string(string))
                        },
                        Err(err) => cx.throw_error(format!("Error processing dump: {}", err)),
                    }
                })
            }
            Err(err) => deferred.settle_with(&channel, move |mut cx| {
                let x: NeonResult<Handle<JsValue>> =
                    cx.throw_error(format!("{} - Error reading dump: {}", err.name(), err));
                x
            }),
        };
    });

    Ok(promise)
}

fn print_minidump_dump<'a, T, W>(
    dump: &Minidump<'a, T>,
    output: &mut W,
    brief: bool,
) -> std::io::Result<()>
where
    T: Deref<Target = [u8]> + 'a,
    W: Write,
{
    dump.print(output)?;

    // Other streams depend on these, so load them upfront.
    let system_info = dump.get_stream::<MinidumpSystemInfo>().ok();
    let memory_list = dump.get_stream::<MinidumpMemoryList<'_>>().ok();
    let memory64_list = dump.get_stream::<MinidumpMemory64List<'_>>().ok();
    let misc_info = dump.get_stream::<MinidumpMiscInfo>().ok();

    if let Ok(thread_list) = dump.get_stream::<MinidumpThreadList<'_>>() {
        thread_list.print(
            output,
            memory_list.as_ref(),
            system_info.as_ref(),
            misc_info.as_ref(),
            brief,
        )?;
    }
    if let Ok(module_list) = dump.get_stream::<MinidumpModuleList>() {
        module_list.print(output)?;
    }
    if let Ok(module_list) = dump.get_stream::<MinidumpUnloadedModuleList>() {
        module_list.print(output)?;
    }
    if let Some(memory_list) = memory_list {
        memory_list.print(output, brief)?;
    }
    if let Some(memory64_list) = memory64_list {
        memory64_list.print(output, brief)?;
    }
    if let Ok(memory_info_list) = dump.get_stream::<MinidumpMemoryInfoList<'_>>() {
        memory_info_list.print(output)?;
    }
    if let Ok(exception) = dump.get_stream::<MinidumpException>() {
        exception.print(output, system_info.as_ref(), misc_info.as_ref())?;
    }
    if let Ok(assertion) = dump.get_stream::<MinidumpAssertion>() {
        assertion.print(output)?;
    }
    if let Some(system_info) = system_info {
        system_info.print(output)?;
    }
    if let Some(misc_info) = misc_info {
        misc_info.print(output)?;
    }
    if let Ok(breakpad_info) = dump.get_stream::<MinidumpBreakpadInfo>() {
        breakpad_info.print(output)?;
    }
    if let Ok(thread_names) = dump.get_stream::<MinidumpThreadNames>() {
        thread_names.print(output)?;
    }
    match dump.get_stream::<MinidumpCrashpadInfo>() {
        Ok(crashpad_info) => crashpad_info.print(output)?,
        Err(Error::StreamNotFound) => (),
        Err(_) => write!(output, "MinidumpCrashpadInfo cannot print invalid data")?,
    }

    // Handle Linux streams that are just a dump of some system "file".
    macro_rules! streams {
        ( $( $x:ident ),* ) => {
            &[$( ( minidump_common::format::MINIDUMP_STREAM_TYPE::$x, stringify!($x) ) ),*]
        };
    }
    fn print_raw_stream<T: Write>(name: &str, contents: &[u8], out: &mut T) -> std::io::Result<()> {
        writeln!(out, "Stream {}:", name)?;
        let s = contents
            .split(|&v| v == 0)
            .map(String::from_utf8_lossy)
            .collect::<Vec<_>>()
            .join("\\0\n");
        write!(out, "{}\n\n", s)
    }

    for &(stream, name) in streams!(
        LinuxCmdLine,
        LinuxEnviron,
        LinuxLsbRelease,
        LinuxProcStatus,
        LinuxCpuInfo,
        LinuxMaps
    ) {
        if let Ok(contents) = dump.get_raw_stream(stream as u32) {
            print_raw_stream(name, contents, output)?;
        }
    }

    Ok(())
}


#[neon::main]
fn main(mut cx: ModuleContext) -> NeonResult<()> {
    cx.export_function("minidumpStackwalk", minidump_stackwalk)?;
    cx.export_function("minidumpDump", minidump_dump)?;
    Ok(())
}
