//! Command-line camera placement and screenshot capture, so a map location
//! can be inspected and compared with the game without flying there.

use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};

use super::*;

/// Seconds after start-up before the screenshot is taken, giving textures
/// time to upload.
const CAPTURE_DELAY: f32 = 8.0;

/// Where to start the camera and whether to capture the first view.
#[derive(Clone, Debug, Default)]
pub struct ViewOptions {
    /// Camera position in Unreal coordinates.
    pub camera: Option<[f32; 3]>,
    /// Point the camera looks at, in Unreal coordinates.
    pub look_at: Option<[f32; 3]>,
    /// PNG written from the window once the scene has loaded; the viewer then
    /// exits.
    pub screenshot: Option<PathBuf>,
}

#[derive(Resource)]
pub(super) struct ViewRequest(pub(super) ViewOptions);

pub(super) struct CapturePlugin;

impl Plugin for CapturePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostStartup, place_requested_camera)
            .add_systems(Update, capture_requested_screenshot);
    }
}

fn place_requested_camera(
    request: Res<ViewRequest>,
    scene: Res<MapScene>,
    mut cameras: Query<(&mut Transform, &mut FlyCamera)>,
) {
    let viewer = |point: [f32; 3]| {
        (scene::unreal_direction_vec(Vec3::from_array(point)) - scene.origin)
            / UNREAL_UNITS_PER_METER
    };
    let Some(position) = request.0.camera.map(viewer) else {
        return;
    };
    for (mut transform, mut camera) in &mut cameras {
        transform.translation = position;
        if let Some(target) = request.0.look_at.map(viewer) {
            let (yaw, pitch) = yaw_pitch_towards(target - position);
            camera.yaw = yaw;
            camera.pitch = pitch;
        }
    }
}

/// The fly camera's yaw and pitch that look along `direction`.
fn yaw_pitch_towards(direction: Vec3) -> (f32, f32) {
    let direction = direction.normalize_or(Vec3::NEG_Z);
    let yaw = (-direction.x).atan2(-direction.z);
    let pitch = direction.y.clamp(-1.0, 1.0).asin().clamp(-1.54, 1.54);
    (yaw, pitch)
}

fn capture_requested_screenshot(
    mut commands: Commands,
    request: Res<ViewRequest>,
    time: Res<Time>,
    mut requested: Local<bool>,
) {
    let Some(path) = request.0.screenshot.clone() else {
        return;
    };
    if *requested || time.elapsed_secs() < CAPTURE_DELAY {
        return;
    }
    *requested = true;
    commands.spawn(Screenshot::primary_window()).observe(
        move |captured: On<ScreenshotCaptured>, mut exit: MessageWriter<AppExit>| {
            match save_png(&path, &captured.image) {
                Ok(()) => eprintln!("viewer: saved screenshot {}", path.display()),
                Err(error) => eprintln!("viewer: cannot save screenshot: {error}"),
            }
            exit.write(AppExit::Success);
        },
    );
}

fn save_png(path: &Path, image: &Image) -> Result<(), String> {
    let width = image.texture_descriptor.size.width;
    let height = image.texture_descriptor.size.height;
    let data = image.data.as_ref().ok_or("the screenshot has no pixel data")?;
    let mut rgba = data.clone();
    match image.texture_descriptor.format {
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => {}
        TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb => {
            rgba.chunks_exact_mut(4).for_each(|pixel| pixel.swap(0, 2));
        }
        format => return Err(format!("unsupported screenshot format {format:?}")),
    }
    if rgba.len() != width as usize * height as usize * 4 {
        return Err("screenshot size does not match its pixel data".into());
    }
    fs::write(path, encode_png(width, height, &rgba))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// Encodes 8-bit RGBA pixels as a PNG with stored (uncompressed) deflate
/// blocks, which needs no compression library.
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(rgba.len() + height as usize);
    for row in rgba.chunks_exact(width as usize * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut zlib = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65_535).peekable();
    if blocks.peek().is_none() {
        zlib.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(block) = blocks.next() {
        zlib.push(u8::from(blocks.peek().is_none()));
        let length = block.len() as u16;
        zlib.extend_from_slice(&length.to_le_bytes());
        zlib.extend_from_slice(&(!length).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, body) in [(b"IHDR", header), (b"IDAT", zlib), (b"IEND", Vec::new())] {
        png.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let start = png.len();
        png.extend_from_slice(kind);
        png.extend_from_slice(&body);
        let crc = crc32(&png[start..]);
        png.extend_from_slice(&crc.to_be_bytes());
    }
    png
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in bytes.chunks(5_552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65_521;
        b %= 65_521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_checksums_match_reference_values() {
        assert_eq!(crc32(b"IEND"), 0xae42_6082);
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
    }

    #[test]
    fn encoded_png_has_signature_header_and_end_chunk() {
        let png = encode_png(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[16..24], &[0, 0, 0, 2, 0, 0, 0, 1]);
        assert_eq!(&png[png.len() - 12..], b"\0\0\0\0IEND\xae\x42\x60\x82");
    }

    #[test]
    fn camera_looking_north_and_down_has_matching_yaw_and_pitch() {
        let (yaw, pitch) = yaw_pitch_towards(Vec3::new(0.0, -1.0, -1.0));
        assert!(yaw.abs() < 1e-6);
        assert!((pitch + std::f32::consts::FRAC_PI_4).abs() < 1e-6);
    }
}
