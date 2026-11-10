//! GPU resize and BGRA -> NV12 conversion before hardware encoder submission.
use std::mem::ManuallyDrop;
use windows::{
    core::{Interface, Result},
    Win32::{
        Foundation::RECT,
        Graphics::{Direct3D11::*, Dxgi::Common::*},
    },
};
pub struct Converter {
    device: ID3D11Device,
    video: ID3D11VideoDevice,
    context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    width: u32,
    height: u32,
}
impl Converter {
    pub unsafe fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        width: u32,
        height: u32,
        out_width: u32,
        out_height: u32,
    ) -> Result<Self> {
        let video: ID3D11VideoDevice = device.cast()?;
        let context: ID3D11VideoContext = context.cast()?;
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            InputWidth: width,
            InputHeight: height,
            OutputFrameRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            OutputWidth: out_width,
            OutputHeight: out_height,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        let enumerator = video.CreateVideoProcessorEnumerator(&desc)?;
        let processor = video.CreateVideoProcessor(&enumerator, 0)?;
        let source = RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        let output = RECT {
            left: 0,
            top: 0,
            right: out_width as i32,
            bottom: out_height as i32,
        };
        let target = fit_rect(width, height, out_width, out_height);
        context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&source));
        context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&target));
        context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&output));
        let black = D3D11_VIDEO_COLOR {
            Anonymous: D3D11_VIDEO_COLOR_0 {
                RGBA: D3D11_VIDEO_COLOR_RGBA {
                    R: 0.0,
                    G: 0.0,
                    B: 0.0,
                    A: 1.0,
                },
            },
        };
        context.VideoProcessorSetOutputBackgroundColor(&processor, false, &black);
        Ok(Self {
            device: device.clone(),
            video,
            context,
            enumerator,
            processor,
            width: out_width,
            height: out_height,
        })
    }
    /// A fixed-capacity input surface lets live resize update only cheap crop
    /// and destination rectangles, without rebuilding the video processor.
    pub unsafe fn set_source_size(&self, width: u32, height: u32) {
        let source = RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        let target = fit_rect(width, height, self.width, self.height);
        self.context
            .VideoProcessorSetStreamSourceRect(&self.processor, 0, true, Some(&source));
        self.context
            .VideoProcessorSetStreamDestRect(&self.processor, 0, true, Some(&target));
    }
    pub unsafe fn convert(&self, source: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: self.width,
            Height: self.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            ..Default::default()
        };
        let mut target = None;
        self.device
            .CreateTexture2D(&desc, None, Some(&mut target))?;
        let target = target.ok_or_else(windows::core::Error::from_thread)?;
        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        let (mut input, mut output) = (None, None);
        self.video.CreateVideoProcessorInputView(
            source,
            &self.enumerator,
            &input_desc,
            Some(&mut input),
        )?;
        self.video.CreateVideoProcessorOutputView(
            &target,
            &self.enumerator,
            &output_desc,
            Some(&mut output),
        )?;
        let output = output.ok_or_else(windows::core::Error::from_thread)?;
        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(input),
            ..Default::default()
        };
        let result = self.context.VideoProcessorBlt(
            &self.processor,
            &output,
            0,
            std::slice::from_ref(&stream),
        );
        ManuallyDrop::drop(&mut stream.pInputSurface);
        result?;
        Ok(target)
    }
}

fn fit_rect(width: u32, height: u32, out_width: u32, out_height: u32) -> RECT {
    let factor = (out_width as f64 / width as f64).min(out_height as f64 / height as f64);
    let w = ((width as f64 * factor).round() as u32 & !1)
        .max(2)
        .min(out_width);
    let h = ((height as f64 * factor).round() as u32 & !1)
        .max(2)
        .min(out_height);
    let left = ((out_width - w) / 2) & !1;
    let top = ((out_height - h) / 2) & !1;
    RECT {
        left: left as i32,
        top: top as i32,
        right: (left + w) as i32,
        bottom: (top + h) as i32,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fit_preserves_aspect_inside_fixed_canvas() {
        for (w, h) in [(1920, 1080), (640, 480), (2560, 1080), (720, 1280)] {
            let r = fit_rect(w, h, 1920, 1080);
            assert!(r.left >= 0 && r.top >= 0 && r.right <= 1920 && r.bottom <= 1080);
            assert_eq!((r.left | r.top | r.right | r.bottom) & 1, 0);
            assert!(
                (f64::from(r.right - r.left) / f64::from(r.bottom - r.top) - w as f64 / h as f64)
                    .abs()
                    < 0.01
            );
        }
    }
}
