use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use rsmpeg::avcodec::{AVCodec, AVCodecContext};
use rsmpeg::avformat::{
    AVFormatContextInput, AVFormatContextOutput, AVIOContextContainer, AVIOContextCustom,
};
use rsmpeg::avutil::{AVFrame, AVMem};
use rsmpeg::error::RsmpegError;
use rsmpeg::ffi;
use rsmpeg::swscale::SwsContext;

struct AVFrameIter {
    frame_buffer: AVFrame,
    format_context: AVFormatContextInput,
    decode_context: AVCodecContext,
    stream_index: usize,
}

impl AVFrameIter {
    fn next_frame(&mut self) -> Result<Option<&mut AVFrame>> {
        loop {
            let packet = loop {
                match self.format_context.read_packet()? {
                    Some(x) if x.stream_index != self.stream_index as i32 => {}
                    x => break x,
                }
            };

            match self.decode_context.send_packet(packet.as_ref()) {
                Ok(_) | Err(RsmpegError::DecoderFlushedError) => {}
                Err(e) => return Err(e.into()),
            };

            match self.decode_context.receive_frame() {
                Ok(frame) => {
                    self.frame_buffer = frame;

                    break Ok(Some(&mut self.frame_buffer));
                }
                Err(RsmpegError::DecoderDrainError) => {}
                Err(RsmpegError::DecoderFlushedError) => break Ok(None),
                Err(e) => break Err(e.into()),
            }
        }
    }
}

fn decode_video(input_format_context: AVFormatContextInput) -> Result<AVFrameIter> {
    let (stream_index, decode_context) = {
        let (stream_index, decoder) = input_format_context
            .find_best_stream(ffi::AVMediaType_AVMEDIA_TYPE_VIDEO)?
            .context("Failed to find the best stream")?;
        let stream = input_format_context.streams().get(stream_index).unwrap();

        let mut decode_context = AVCodecContext::new(&decoder);
        decode_context.apply_codecpar(&stream.codecpar())?;
        decode_context.open(None)?;
        decode_context.set_framerate(stream.avg_frame_rate);
        decode_context.set_time_base(stream.time_base);

        (stream_index, decode_context)
    };

    Ok(AVFrameIter {
        frame_buffer: AVFrame::new(),
        format_context: input_format_context,
        decode_context,
        stream_index,
    })
}

#[allow(clippy::type_complexity)]
fn io_context_custom(
    data: Vec<u8>,
    write: bool,
) -> Result<(AVIOContextCustom, Arc<Mutex<Cursor<Vec<u8>>>>)> {
    let data = Arc::new(Mutex::new(Cursor::new(data)));

    let seek = {
        let data = data.clone();
        Box::new(move |_: &mut Vec<u8>, offset: i64, whence: i32| {
            let mut data = data.lock().unwrap();
            const AVSEEK_SIZE: i32 = ffi::AVSEEK_SIZE as i32;
            match whence {
                0 => data.seek(SeekFrom::Start(offset as _)),
                1 => data.seek(SeekFrom::Current(offset)),
                2 => data.seek(SeekFrom::End(offset)),
                AVSEEK_SIZE => return data.get_ref().len() as _,
                _ => return -1,
            }
            .map(|x| x as _)
            .unwrap_or(-1)
        })
    };

    let io_context = if write {
        let write_packet = {
            let data = data.clone();
            Box::new(
                move |_: &mut Vec<u8>, buf: &[u8]| match data.lock().unwrap().write_all(buf) {
                    Ok(_) => buf.len() as _,
                    Err(_) => -1,
                },
            )
        };

        AVIOContextCustom::alloc_context(
            AVMem::new(4096),
            true,
            Vec::new(),
            None,
            Some(write_packet),
            Some(seek),
        )
    } else {
        let read_packet = {
            let data = data.clone();
            Box::new(move |_: &mut Vec<u8>, buf: &mut [u8]| {
                let mut data = data.lock().unwrap();
                match data.read(buf) {
                    Ok(0) => ffi::AVERROR_EOF,
                    Ok(n) => n as _,
                    Err(_) => -1,
                }
            })
        };

        AVIOContextCustom::alloc_context(
            AVMem::new(4096),
            false,
            Vec::new(),
            Some(read_packet),
            None,
            Some(seek),
        )
    };

    Ok((io_context, data))
}

fn input_format_context(data: Vec<u8>) -> Result<AVFormatContextInput> {
    let (io_context, _) = io_context_custom(data, false)?;
    let input_format_context =
        AVFormatContextInput::from_io_context(AVIOContextContainer::Custom(io_context))?;

    Ok(input_format_context)
}

#[allow(clippy::type_complexity)]
fn output_format_context() -> Result<(AVFormatContextOutput, Arc<Mutex<Cursor<Vec<u8>>>>)> {
    let (io_context, data) = io_context_custom(Vec::new(), true)?;
    let output_format_context =
        AVFormatContextOutput::create(c".mp4", Some(AVIOContextContainer::Custom(io_context)))?;

    Ok((output_format_context, data))
}

fn encode_mp4(mut src: AVFrameIter) -> Result<Vec<u8>> {
    let buffer = {
        let time_base = src.decode_context.time_base;
        let framerate = src.decode_context.framerate;
        let first_frame = src.next_frame()?.context("Failed to get first frame")?;
        let width = first_frame.width;
        let height = first_frame.height;

        let (mut output_format_context, buffer) = output_format_context()?;

        let encoder =
            AVCodec::find_encoder_by_name(c"libx264").context("Failed to find encoder codec")?;
        let mut encode_context = AVCodecContext::new(&encoder);
        encode_context.set_width(width);
        encode_context.set_height(height);
        encode_context.set_time_base(time_base);
        encode_context.set_framerate(framerate);
        encode_context.set_pix_fmt(ffi::AVPixelFormat_AV_PIX_FMT_YUV420P);
        unsafe {
            if ffi::av_opt_set(
                encode_context.priv_data,
                c"preset".as_ptr(),
                c"slow".as_ptr(),
                0,
            ) < 0
            {
                bail!("Failed to set preset");
            }
        }
        if output_format_context.oformat().flags & ffi::AVFMT_GLOBALHEADER as i32 != 0 {
            encode_context
                .set_flags(encode_context.flags | ffi::AV_CODEC_FLAG_GLOBAL_HEADER as i32);
        }
        encode_context.open(None)?;

        let mut dst_frame = AVFrame::new();
        dst_frame.set_format(encode_context.pix_fmt);
        dst_frame.set_width(encode_context.width);
        dst_frame.set_height(encode_context.height);
        dst_frame.alloc_buffer()?;

        {
            let mut out_stream = output_format_context.new_stream();
            out_stream.set_codecpar(encode_context.extract_codecpar());
        }

        output_format_context.write_header(&mut None)?;

        let mut sws_context = SwsContext::get_context(
            width,
            height,
            first_frame.format,
            width,
            height,
            encode_context.pix_fmt,
            ffi::SWS_FAST_BILINEAR | ffi::SWS_ACCURATE_RND,
        )
        .context("Failed to get sws_context")?;
        let mut encode_frame = |src_frame: &mut AVFrame| -> Result<()> {
            let frame_after = if src_frame.format == dst_frame.format {
                src_frame
            } else {
                sws_context.scale_frame(src_frame, 0, height, &mut dst_frame)?;
                dst_frame.set_pts(src_frame.pts);
                &mut dst_frame
            };

            encode_write_frame(
                Some(frame_after),
                &mut encode_context,
                &mut output_format_context,
                0,
            )
        };
        encode_frame(first_frame)?;
        while let Some(src_frame) = src.next_frame()? {
            encode_frame(src_frame)?;
        }

        encode_write_frame(None, &mut encode_context, &mut output_format_context, 0)?;
        output_format_context.write_trailer()?;

        buffer
    };

    let ret = Arc::into_inner(buffer)
        .context("Failed to get buffer")?
        .into_inner()?
        .into_inner();

    Ok(ret)
}

fn encode_write_frame(
    frame_after: Option<&AVFrame>,
    encode_context: &mut AVCodecContext,
    output_format_context: &mut AVFormatContextOutput,
    out_stream_index: usize,
) -> Result<()> {
    encode_context.send_frame(frame_after)?;

    loop {
        let mut packet = match encode_context.receive_packet() {
            Ok(packet) => packet,
            Err(RsmpegError::EncoderDrainError) | Err(RsmpegError::EncoderFlushedError) => break,
            Err(e) => return Err(e.into()),
        };

        packet.set_stream_index(out_stream_index as i32);
        packet.rescale_ts(
            encode_context.time_base,
            output_format_context
                .streams()
                .get(out_stream_index)
                .context("Failed to get stream")?
                .time_base,
        );

        match output_format_context.interleaved_write_frame(&mut packet) {
            Ok(()) => Ok(()),
            Err(RsmpegError::InterleavedWriteFrameError(-22)) => Ok(()),
            Err(e) => Err(e),
        }?;
    }

    Ok(())
}

pub fn video_to_mp4(data: Vec<u8>) -> Result<Vec<u8>> {
    let format_context = input_format_context(data)?;
    let frame_iter = decode_video(format_context)?;

    encode_mp4(frame_iter)
}
