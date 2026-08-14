// 最小复现：ffmpeg-next 解码 long.wav 第一包并重采样到 16k mono
fn main() {
    ffmpeg::init().unwrap();
    let ictx = ffmpeg::format::input("/Volumes/Rc20/Projects/FunASR/benchmarks/local_jp_test/audio/long.wav").unwrap();
    let stream = ictx.streams().best(ffmpeg::media::Type::Audio).unwrap();
    println!("stream: ch_layout={:?} rate={:?} format={:?}",
        stream.parameters().channel_layout(),
        stream.parameters().rate(),
        stream.parameters().format());
    let mut dec = ffmpeg::codec::decoder::Audio::from_parameters(stream.parameters()).unwrap();
    println!("decoder: ch_layout={:?} rate={} format={:?}",
        dec.channel_layout(), dec.rate(), dec.format());
    let mut resampler = ffmpeg::software::resampling::Context::get(
        dec.format(), dec.channel_layout(), dec.rate() as u32,
        ffmpeg::format::Sample::I16(ffmpeg::format::sample::Type::Packed),
        ffmpeg::channel_layout::ChannelLayout::MONO, 16000).unwrap();
    println!("resampler OK");
    for (s, pkt) in ictx.packets() {
        if s.index() != stream.index() { continue; }
        dec.send_packet(&pkt).unwrap();
        let mut frame = ffmpeg::frame::Audio::empty();
        while let Ok(_) = dec.receive_frame(&mut frame) {
            println!("frame: ch_layout={:?} rate={} format={:?} samples={}",
                frame.channel_layout(), frame.rate(), frame.format(), frame.samples());
            let mut out = ffmpeg::frame::Audio::empty();
            match resampler.run(&frame, &mut out) {
                Ok(_) => println!("  resample OK, out samples={}", out.samples()),
                Err(e) => println!("  RESAMPLE FAIL: {:?}", e),
            }
            break; // 只看第一帧
        }
        break;
    }
}
