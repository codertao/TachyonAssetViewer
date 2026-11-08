//! Tiny linear algebra: just enough for model placement and an orbit camera.

/// Row-major 3x3 matrix; `mul_vec` computes M·v.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat3(pub [[f32; 3]; 3]);

impl Mat3 {
    pub const IDENTITY: Mat3 = Mat3([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);

    pub fn mul(&self, o: &Mat3) -> Mat3 {
        let mut r = [[0.0; 3]; 3];
        for (i, row) in r.iter_mut().enumerate() {
            for (j, c) in row.iter_mut().enumerate() {
                *c = (0..3).map(|k| self.0[i][k] * o.0[k][j]).sum();
            }
        }
        Mat3(r)
    }

    pub fn mul_vec(&self, v: [f32; 3]) -> [f32; 3] {
        let m = &self.0;
        [
            m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
            m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
            m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
        ]
    }

    pub fn scaled(&self, s: f32) -> Mat3 {
        Mat3(self.0.map(|row| row.map(|c| c * s)))
    }

    /// The engine's Euler→matrix convention, from `math_todo_with_rotation 0x52d9cd` +
    /// `some_rotate_by_global 0x4b24d0`: v' = Ry(a1) · Rx(-a2) · Rz(a3) · v, where (a1, a2, a3)
    /// are the three stored angles (OCF `ANGLES` order: yaw, pitch, roll), in engine axes.
    pub fn from_engine_euler_deg(a: [f32; 3]) -> Mat3 {
        let (s1, c1) = a[0].to_radians().sin_cos();
        let (s2, c2) = a[1].to_radians().sin_cos();
        let (s3, c3) = a[2].to_radians().sin_cos();
        let ry = Mat3([[c1, 0.0, s1], [0.0, 1.0, 0.0], [-s1, 0.0, c1]]);
        // Rx(-a2): y' = y·c + z·s, z' = -y·s + z·c
        let rx = Mat3([[1.0, 0.0, 0.0], [0.0, c2, s2], [0.0, -s2, c2]]);
        let rz = Mat3([[c3, -s3, 0.0], [s3, c3, 0.0], [0.0, 0.0, 1.0]]);
        ry.mul(&rx).mul(&rz)
    }
}

/// Column-major 4x4 (OpenGL layout): element (row r, col c) at `m[c * 4 + r]`.
#[derive(Debug, Clone, Copy)]
pub struct Mat4(pub [f32; 16]);

impl Mat4 {
    pub fn perspective(fovy: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
        let f = 1.0 / (fovy / 2.0).tan();
        let mut m = [0.0; 16];
        m[0] = f / aspect;
        m[5] = f;
        m[10] = (far + near) / (near - far);
        m[11] = -1.0;
        m[14] = 2.0 * far * near / (near - far);
        Mat4(m)
    }

    pub fn look_at(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> Mat4 {
        let f = normalize(sub(target, eye));
        let s = normalize(cross(f, up));
        let u = cross(s, f);
        Mat4([
            s[0], u[0], -f[0], 0.0, //
            s[1], u[1], -f[1], 0.0, //
            s[2], u[2], -f[2], 0.0, //
            -dot(s, eye), -dot(u, eye), dot(f, eye), 1.0,
        ])
    }
}

pub fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}
pub fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
pub fn normalize(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt();
    if l > 1e-12 {
        scale(a, 1.0 / l)
    } else {
        [0.0, 0.0, 1.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-5)
    }

    /// Mirror of `some_rotate_by_global` with the sin/cos layout of `math_todo_with_rotation`
    /// (B = s1, c1, s2; A = c2, s3, c3), to check `from_engine_euler_deg` against the original.
    fn engine_rotate(a: [f32; 3], v: [f32; 3]) -> [f32; 3] {
        let (s1, c1) = a[0].to_radians().sin_cos();
        let (s2, c2) = a[1].to_radians().sin_cos();
        let (s3, c3) = a[2].to_radians().sin_cos();
        let (bx, by, bz, ax, ay, az) = (s1, c1, s2, c2, s3, c3);
        let r1 = v[0] * az - v[1] * ay;
        let mut r2 = v[0] * ay + v[1] * az;
        let y = bz * v[2] + ax * r2;
        r2 = ax * v[2] - bz * r2;
        let z = by * r2 - bx * r1;
        let x = bx * r2 + by * r1;
        [x, y, z]
    }

    #[test]
    fn euler_matches_engine() {
        for a in [[30.0, 0.0, 0.0], [0.0, 40.0, 0.0], [0.0, 0.0, 50.0], [10.0, -70.0, 125.0]] {
            for v in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.3, -2.0, 5.0]] {
                assert!(close(Mat3::from_engine_euler_deg(a).mul_vec(v), engine_rotate(a, v)), "{a:?} {v:?}");
            }
        }
    }
}
