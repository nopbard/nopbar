// Compiled only by offline tests. Never forwards prompts to a real AI.
use std::{
    env,
    io::{self, Read, Write},
    process::{Command, Stdio},
    time::Duration,
};
fn main() {
    if env::current_dir().unwrap().join("record-invocations").exists() {
        let mut f=std::fs::OpenOptions::new().create(true).append(true).open("invocations").unwrap();
        writeln!(f,"child").unwrap();
    }
    let args = env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str){
        Some("--version")=>{
            if env::current_dir().unwrap().join("bad-version").exists(){println!("unrecognized output");}else{println!("2.1.325 (fixture)");}
        },
        Some("--help")=>{
            let help="--safe-mode --tools --strict-mcp-config --no-session-persistence --include-partial-messages --settings --mode --sandbox --disable-slash-commands";
            let cwd=env::current_dir().unwrap();
            if cwd.join("missing-options").exists(){eprintln!("--help --version");}
            else if cwd.join("mixed-help").exists(){let (a,b)=help.split_once("--settings").unwrap();println!("{a}");eprintln!("--settings{b}");}
            else if cwd.join("stderr-help").exists(){eprintln!("{help}");}else{println!("{help}");}
        },
        Some("auth")=>{
            eprintln!("diagnostic warning; this must not corrupt JSON stdout");
            let cwd=env::current_dir().unwrap();let name=cwd.file_name().unwrap().to_string_lossy();
            if name=="unknown"{println!("{{}}");}else{println!("{{\"loggedIn\":{}}}",name!="login");}
        }
        Some("tree")=>{
            let child=Command::new(env::current_exe().unwrap()).arg("sleep").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
            println!("{}",child.id());io::stdout().flush().unwrap();std::thread::sleep(Duration::from_secs(60));
        }
        Some("sleep")=>std::thread::sleep(Duration::from_secs(60)),
        Some("installer")=>{println!("download");io::stdout().flush().unwrap();std::thread::sleep(Duration::from_millis(50));println!("installed");}
        _=>{let mut input=String::new();io::stdin().read_to_string(&mut input).unwrap();
            if env::var("NOPBAR_CHAT").as_deref()!=Ok("1"){std::process::exit(9)}
            println!("{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"fixture reply\"}}");
        }
    }
}
