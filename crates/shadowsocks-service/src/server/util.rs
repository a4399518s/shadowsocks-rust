use std::fs;
use std::fs::File;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use byteorder::WriteBytesExt;
use shadowsocks::ServerConfig;
use crate::server::TcpServer;

pub fn dump_pid(dir: String,id: String,pid:u32){
    let dir = format!("{}/{}",dir,id);
    // 判断文件夹是否存在
    if fs::metadata(&dir).is_err() {
        // 如果文件夹不存在，则创建
        fs::create_dir_all(&dir).expect("TODO: panic message");
    }

    let mut pidFile = fs::File::create(format!("{dir}/pid")).unwrap();
    writeln!(pidFile, "{}", pid).expect("TODO: panic message");
}

pub fn dump_info(dir: &String,id: &String,pid:u32,useUpSum:&Arc<AtomicU64>,useDownSum:&Arc<AtomicU64>){
    let pidDir = format!("{dir}/{id}/{pid}");
    // 判断文件夹是否存在
    if fs::metadata(&pidDir).is_err() {
        // 如果文件夹不存在，则创建
        fs::create_dir_all(&pidDir).expect("TODO: panic message");
    }
    let mut useDownSumFile = fs::File::create(format!("{pidDir}/useDownSum")).unwrap();
    writeln!(useDownSumFile, "{}", useDownSum.load(Ordering::Relaxed)).expect("TODO: panic message");
    let mut useUpSumFile = fs::File::create(format!("{pidDir}/useUpSum")).unwrap();
    writeln!(useUpSumFile, "{}", useUpSum.load(Ordering::Relaxed)).expect("TODO: panic message");

}