use glam::{Mat4, Vec3};

use crate::{HEIGHT, InputState, WIDTH};

const DEFAULT_FOV: f32 = 90.0;
const DEFAULD_NEAR_Z: f32 = 0.1;

const MOUSE_SENSITIVITY: f32 = 0.5;
const DEFAULT_CAMERA_SPEED: f32 = 50.0;
const MIN_CAMERA_SPEED: f32 = 0.25;
const MAX_CAMERA_SPEED: f32 = 4000.0;
const WHEEL_SPEED_FACTOR: f32 = 1.25;

pub struct Camera {
    pos: Vec3,
    forward: Vec3,
    world_to_view: Mat4,
    view_to_clip: Mat4,
}

impl Camera {
    pub fn new(pos: Vec3) -> Self {
        let aspect_ratio = WIDTH as f32 / HEIGHT as f32;

        Self {
            pos,
            forward: Vec3::Y,
            world_to_view: Mat4::IDENTITY,
            view_to_clip: Mat4::perspective_infinite_reverse_lh(DEFAULT_FOV.to_radians(), aspect_ratio, DEFAULD_NEAR_Z),
        }
    }

    pub fn pos(&self) -> &Vec3 {
        &self.pos
    }

    pub fn forward(&self) -> &Vec3 {
        &self.forward
    }

    pub fn world_to_clip(&self) -> Mat4 {
        self.view_to_clip * self.world_to_view
    }
}

pub struct CameraController {
    speed: f32,
    yaw: f32,
    pitch: f32,
}

impl CameraController {
    pub fn control(&mut self, dt: f32, input: &InputState, camera: &mut Camera) {
        if input.right_mouse_down {
            self.yaw += input.mouse_dx as f32 * MOUSE_SENSITIVITY;
            self.pitch += input.mouse_dy as f32 * MOUSE_SENSITIVITY;

            self.yaw = self.yaw.rem_euclid(360.0);
            self.pitch = self.pitch.clamp(-89.0, 89.0);

            if input.mouse_wheel_delta != 0 {
                self.speed *= WHEEL_SPEED_FACTOR.powf(input.mouse_wheel_delta as f32);
                self.speed = self.speed.clamp(MIN_CAMERA_SPEED, MAX_CAMERA_SPEED);
            }
        }

        let up = Vec3::Y;
        let forward = self.forward();
        let right = up.cross(forward).normalize();

        let mut movement = Vec3::ZERO;

        if input.keys[b'W' as usize] {
            movement += forward;
        }

        if input.keys[b'S' as usize] {
            movement -= forward;
        }

        if input.keys[b'A' as usize] {
            movement -= right;
        }

        if input.keys[b'D' as usize] {
            movement += right;
        }

        if input.keys[b'E' as usize] {
            movement += up;
        }

        if input.keys[b'Q' as usize] {
            movement -= up;
        }

        if movement.length_squared() > 0.0 {
            movement = movement.normalize();
            camera.pos += movement * self.speed * dt;
        }

        camera.forward = forward;
        camera.world_to_view = Mat4::look_to_lh(camera.pos, forward, up);
    }

    fn forward(&self) -> Vec3 {
        let yaw_rad = self.yaw.to_radians();
        let pitch_rad = self.pitch.to_radians();

        let forward = Vec3::new(
            yaw_rad.cos() * pitch_rad.cos(),
            pitch_rad.sin(),
            yaw_rad.sin() * pitch_rad.cos(),
        );

        forward.normalize()
    }
}

impl Default for CameraController {
    fn default() -> Self {
        Self {
            speed: DEFAULT_CAMERA_SPEED, // meters per sec
            yaw: -90.0,
            pitch: 0.0,
        }
    }
}
