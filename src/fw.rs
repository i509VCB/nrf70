use core::mem;

use defmt::panic;
use num_enum::TryFromPrimitive;

use crate::c::{self, image_ids};

pub struct FirmwareInfo<'a> {
    pub images: [Option<FirmwareImage<'a>>; 4],
    pub signature: u32,
    pub features: c::feature_flags,
}

impl<'a> FirmwareInfo<'a> {
    pub fn read(data: &'a [u8]) -> Self {
        debug_assert_eq!(
            mem::size_of::<c::fw_image>(),
            mem::size_of::<fw_image_verify>(),
            "fw_image layout has changed"
        );
        debug_assert_eq!(
            mem::size_of::<c::fw_image_info>(),
            mem::size_of::<fw_image_info_verify>(),
            "fw_image_info layout has changed"
        );

        let mut cursor = Cursor { data, cursor: 0 };

        // This must match the layout of c::fw_image_info.
        let signature = cursor.read_u32();
        let num_images = cursor.read_u32();
        let version = cursor.read_u32();
        let feature_flags = cursor.read_u32();
        let len = cursor.read_u32();
        let hash = cursor.read_slice::<{ c::PATCH_HASH_LEN as usize }>();

        const RPU_VERSION: u32 =
            c::RPU_FAMILY << 24 | c::RPU_MAJOR_VERSION << 16 | c::RPU_MINOR_VERSION << 8 | c::RPU_PATCH_VERSION;
        debug_assert_eq!(
            version, RPU_VERSION,
            "Parsed firmware version and RPU version from headers does not match"
        );
        debug_assert_eq!(signature, c::PATCH_SIGNATURE, "Patch signature does not match");
        debug_assert_eq!(num_images, c::PATCH_NUM_IMAGES, "Number of patch images does not match");

        // TODO: Feature flags (e.g. do not use a radio test firmware with a system config)

        let mut images = [const { None }; 4];

        for i in 0..num_images {
            images[i as usize] = Some(FirmwareImage::read(&mut cursor));
        }

        let mut expected_len = mem::size_of::<c::fw_image_info>();

        for image in images.iter() {
            if let Some(image) = image {
                expected_len += mem::size_of::<c::fw_image>();
                expected_len += image.data.len();
            }
        }

        debug_assert_eq!(cursor.cursor, expected_len, "Sizes do not add up");

        Self {
            images,
            signature,
            features: c::feature_flags::FEAT_SYSTEM_MODE,
        }
    }

    pub fn get(&self, id: image_ids) -> &'a [u8] {
        for image in self.images.iter() {
            if let Some(image) = image {
                if image.ty == id {
                    return image.data;
                }
            }
        }

        panic!("Could not find image of type")
    }
}

pub struct FirmwareImage<'a> {
    pub data: &'a [u8],
    pub ty: c::image_ids,
}

impl<'a: 'b, 'b> FirmwareImage<'a> {
    fn read(cursor: &'b mut Cursor<'a>) -> Self {
        // This must match the layout of c::fw_image.
        let ty = cursor.read_u32();
        let len = cursor.read_u32();
        let data = cursor.get_slice(len as usize);

        Self {
            data,
            ty: c::image_ids::try_from_primitive(ty).expect("Invalid image id type"),
        }
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    cursor: usize,
}

impl<'a> Cursor<'a> {
    fn read_slice<const N: usize>(&mut self) -> [u8; N] {
        let mut bytes = [0; N];
        bytes.copy_from_slice(&self.data[self.cursor..self.cursor + N]);
        self.cursor += N;
        bytes
    }

    fn read_u32(&mut self) -> u32 {
        u32::from_ne_bytes(self.read_slice::<4>())
    }

    /// Get a slice and add to the cursor's position.
    fn get_slice(&mut self, len: usize) -> &'a [u8] {
        let slice = &self.data[self.cursor..self.cursor + len];
        self.cursor += len;
        slice
    }
}

// For verification.
#[allow(non_camel_case_types)]
#[repr(C, packed)]
pub struct fw_image_verify {
    pub type_: ::core::ffi::c_uint,
    pub len: ::core::ffi::c_uint,
}

#[allow(non_camel_case_types)]
#[repr(C, packed)]
pub struct fw_image_info_verify {
    pub signature: ::core::ffi::c_uint,
    pub num_images: ::core::ffi::c_uint,
    pub version: ::core::ffi::c_uint,
    pub feature_flags: ::core::ffi::c_uint,
    pub len: ::core::ffi::c_uint,
    pub hash: [::core::ffi::c_uchar; 32usize],
}

#[cfg(test)]
mod tests {
    use super::FirmwareInfo;
    use crate::FW;

    #[test]
    fn read_fw() {
        let _info = FirmwareInfo::read(FW);
    }
}
