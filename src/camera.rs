//! Orbit camera. Everything is f64; the GPU matrix is computed relative to a scene origin.

use glam::{DMat3, DMat4, DQuat, DVec3, Mat4};

#[derive(Clone, Debug)]
pub struct Camera {
    pub target: DVec3,
    pub dist: f64,
    /// Camera orientation: eye = target + rot * (0,0,dist), up = rot * (0,1,0)
    pub rot: DQuat,
    pub fov_deg: f64,
    pub scene_radius: f64,
}

#[derive(Clone, Copy, PartialEq)]
pub enum StandardView {
    Isometric,
    Front,
    Right,
    Top,
}

impl Camera {
    pub fn new(center: DVec3, radius: f64) -> Self {
        let mut c = Camera { target: center, dist: 1.0, rot: DQuat::IDENTITY, fov_deg: 30.0, scene_radius: radius };
        c.set_standard(StandardView::Isometric, center, radius);
        c
    }

    fn look_rot(dir_to_eye: DVec3, up: DVec3) -> DQuat {
        let z = dir_to_eye.normalize();
        let mut x = up.cross(z);
        if x.length_squared() < 1e-12 {
            x = DVec3::X;
        }
        let x = x.normalize();
        let y = z.cross(x);
        DQuat::from_mat3(&DMat3::from_cols(x, y, z))
    }

    pub fn fit_dist(&self, radius: f64) -> f64 {
        (radius.max(1e-12) / (self.fov_deg.to_radians() * 0.5).sin()) * 1.1
    }

    pub fn set_standard(&mut self, v: StandardView, center: DVec3, radius: f64) {
        let (dir, up) = match v {
            StandardView::Isometric => (DVec3::new(1.0, 1.0, 1.0), DVec3::Z),
            StandardView::Front => (DVec3::new(0.0, -1.0, 0.0), DVec3::Z),
            StandardView::Right => (DVec3::new(1.0, 0.0, 0.0), DVec3::Z),
            StandardView::Top => (DVec3::new(0.0, 0.0, 1.0), DVec3::Y),
        };
        self.rot = Self::look_rot(dir, up);
        self.target = center;
        self.scene_radius = radius;
        self.dist = self.fit_dist(radius);
    }

    pub fn fit(&mut self, center: DVec3, radius: f64) {
        self.target = center;
        self.scene_radius = radius;
        self.dist = self.fit_dist(radius);
    }

    pub fn eye(&self) -> DVec3 {
        self.target + self.rot * DVec3::new(0.0, 0.0, self.dist)
    }

    pub fn up(&self) -> DVec3 {
        self.rot * DVec3::Y
    }

    pub fn right(&self) -> DVec3 {
        self.rot * DVec3::X
    }

    /// Drag in pixels: the model follows the cursor.
    pub fn orbit(&mut self, dx: f64, dy: f64) {
        let k = 0.01;
        self.rot = (self.rot * DQuat::from_rotation_y(-dx * k) * DQuat::from_rotation_x(-dy * k)).normalize();
    }

    pub fn spin(&mut self, dx: f64) {
        self.rot = (self.rot * DQuat::from_rotation_z(dx * 0.01)).normalize();
    }

    pub fn pan(&mut self, dx: f64, dy: f64, height_px: f64) {
        let s = 2.0 * self.dist * (self.fov_deg.to_radians() * 0.5).tan() / height_px.max(1.0);
        self.target += -self.right() * dx * s + self.up() * dy * s;
    }

    pub fn zoom(&mut self, scroll: f64) {
        self.dist = (self.dist * (-scroll * 0.0015).exp()).max(1e-9);
    }

    /// View-projection matrix for geometry expressed relative to `origin`.
    pub fn mvp(&self, aspect: f64, origin: DVec3) -> Mat4 {
        let eye = self.eye() - origin;
        let target = self.target - origin;
        let z = (eye - target).normalize();
        let x = self.right();
        let y = self.up();
        let view = DMat4::from_cols(
            glam::DVec4::new(x.x, y.x, z.x, 0.0),
            glam::DVec4::new(x.y, y.y, z.y, 0.0),
            glam::DVec4::new(x.z, y.z, z.z, 0.0),
            glam::DVec4::new(-x.dot(eye), -y.dot(eye), -z.dot(eye), 1.0),
        );
        let near = (self.dist - 1.5 * self.scene_radius).max(self.dist * 0.01);
        let far = self.dist + 1.5 * self.scene_radius;
        let f = 1.0 / (self.fov_deg.to_radians() * 0.5).tan();
        let proj = DMat4::from_cols(
            glam::DVec4::new(f / aspect.max(1e-6), 0.0, 0.0, 0.0),
            glam::DVec4::new(0.0, f, 0.0, 0.0),
            glam::DVec4::new(0.0, 0.0, far / (near - far), -1.0),
            glam::DVec4::new(0.0, 0.0, near * far / (near - far), 0.0),
        );
        (proj * view).as_mat4()
    }

    pub fn to_json(&self) -> [[f64; 3]; 3] {
        [self.eye().to_array(), self.target.to_array(), self.up().to_array()]
    }

    pub fn from_json(&mut self, v: [[f64; 3]; 3]) {
        let eye = DVec3::from_array(v[0]);
        let target = DVec3::from_array(v[1]);
        let up = DVec3::from_array(v[2]);
        let d = eye - target;
        if d.length() > 1e-12 {
            self.target = target;
            self.dist = d.length();
            self.rot = Self::look_rot(d, up);
        }
    }

    /// Screen-space (x right, y down) direction of a world axis, for the axes triad.
    pub fn axis_screen(&self, a: DVec3) -> [f32; 2] {
        let c = self.rot.inverse() * a;
        [c.x as f32, -c.y as f32]
    }
}
