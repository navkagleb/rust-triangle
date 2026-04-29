use glam::{Mat4, Vec3};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_SPACE;

use crate::{HEIGHT, InputState, WIDTH};

const SENSITIVIY: f32 = 0.1;
const SPEED: f32 = 0.1;

pub struct Camera {
    position: Vec3,
    world_to_view: Mat4,
    view_to_clip: Mat4,
}

impl Camera {
    pub fn new() -> Self {
        let fov_y = 90_f32.to_radians();
        let aspect_ratio = WIDTH as f32 / HEIGHT as f32;
        let near_z = 0.1;

        Self {
            position: Vec3::ZERO,
            world_to_view: Mat4::IDENTITY,
            view_to_clip: Mat4::perspective_infinite_reverse_lh(fov_y, aspect_ratio, near_z),
        }
    }

    pub fn position(&self) -> &Vec3 {
        &self.position
    }

    pub fn world_to_clip(&self) -> Mat4 {
        self.view_to_clip * self.world_to_view
    }
}

pub struct CameraController {
    yaw: f32,
    pitch: f32,
}

impl CameraController {
    pub fn control(&mut self, dt: f32, input: &InputState, camera: &mut Camera) {
        if input.right_mouse_down {
            self.yaw += input.mouse_dx as f32 * SENSITIVIY;
            self.pitch += input.mouse_dy as f32 * SENSITIVIY;
            self.pitch = self.pitch.clamp(-89.0, 89.0);
        }

        let yaw_rad = self.yaw.to_radians();
        let pitch_rad = self.pitch.to_radians();

        let front_dir = Vec3::new(
            yaw_rad.cos() * pitch_rad.cos(),
            pitch_rad.sin(),
            yaw_rad.sin() * pitch_rad.cos(),
        )
        .normalize();

        let speed = SPEED * dt;

        if input.keys[b'W' as usize] {
            camera.position += front_dir * speed;
        }

        if input.keys[b'S' as usize] {
            camera.position -= front_dir * speed;
        }

        if input.keys[b'A' as usize] {
            camera.position += front_dir.cross(Vec3::Y).normalize() * speed;
        }

        if input.keys[b'D' as usize] {
            camera.position -= front_dir.cross(Vec3::Y).normalize() * speed;
        }

        if input.keys[VK_SPACE.0 as usize] {
            camera.position.y += speed;
        }

        if input.keys[b'C' as usize] {
            camera.position.y -= speed;
        }

        camera.world_to_view = Mat4::look_to_lh(camera.position, front_dir, Vec3::Y);
    }
}

impl Default for CameraController {
    fn default() -> Self {
        Self { yaw: 0.0, pitch: 0.0 }
    }
}
