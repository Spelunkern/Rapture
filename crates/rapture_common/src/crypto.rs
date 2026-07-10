use aes::Aes128;
use anyhow::{Context, Result};
use cipher::{BlockEncrypt, KeyInit};
use hmac::{Hmac, Mac};
use num_bigint::BigUint;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

const EXPONENT: [u8; 64] = [49; 64];

const PRIVATE_EXPONENT_BIG_ENDIAN: [u8; 128] = [
    8, 138, 199, 186, 85, 15, 58, 210, 200, 61, 195, 4, 184, 179, 34, 206, 136, 195, 217, 130, 183,
    109, 186, 169, 227, 164, 127, 20, 117, 176, 28, 24, 90, 198, 187, 187, 40, 5, 51, 95, 248, 64,
    57, 242, 169, 64, 223, 55, 116, 120, 193, 221, 70, 197, 63, 248, 58, 55, 238, 18, 208, 78, 2,
    45, 37, 179, 75, 13, 162, 144, 149, 59, 224, 204, 34, 103, 167, 209, 57, 110, 0, 49, 210, 152,
    80, 163, 80, 12, 168, 99, 197, 68, 0, 242, 57, 14, 94, 9, 44, 39, 251, 171, 79, 15, 227, 187,
    186, 11, 197, 38, 102, 237, 225, 95, 163, 103, 47, 182, 138, 183, 143, 151, 42, 251, 168, 233,
    0, 103,
];

const MODULUS_BIG_ENDIAN: [u8; 128] = [
    149, 236, 7, 136, 170, 27, 149, 37, 73, 12, 247, 7, 100, 209, 106, 211, 222, 97, 144, 12, 143,
    217, 169, 171, 61, 24, 247, 7, 180, 99, 104, 179, 227, 100, 28, 163, 170, 190, 85, 11, 6, 120,
    254, 97, 221, 101, 188, 248, 39, 66, 9, 148, 75, 146, 151, 125, 69, 18, 217, 2, 45, 11, 228,
    27, 197, 93, 207, 127, 153, 22, 227, 219, 19, 30, 152, 203, 144, 16, 185, 118, 237, 46, 122,
    155, 231, 221, 197, 7, 154, 240, 94, 207, 90, 123, 62, 67, 170, 125, 25, 71, 203, 254, 226,
    181, 2, 215, 94, 248, 172, 203, 189, 98, 164, 48, 193, 67, 93, 248, 138, 163, 249, 113, 186,
    61, 76, 25, 170, 229,
];

pub struct LoginCryptoMaterial {
    pub public_exponent: [u8; 64],
    pub public_modulus: [u8; 128],
}

pub fn login_crypto_material() -> LoginCryptoMaterial {
    let mut public_modulus = MODULUS_BIG_ENDIAN;
    public_modulus.reverse();

    LoginCryptoMaterial {
        public_exponent: EXPONENT,
        public_modulus,
    }
}

pub fn decrypt_login_rsa(encrypted: &[u8]) -> Vec<u8> {
    let mut private_exponent = PRIVATE_EXPONENT_BIG_ENDIAN;
    private_exponent.reverse();

    let mut modulus = MODULUS_BIG_ENDIAN;
    modulus.reverse();

    let encrypted = BigUint::from_bytes_le(encrypted);
    let private_exponent = BigUint::from_bytes_le(&private_exponent);
    let modulus = BigUint::from_bytes_le(&modulus);

    encrypted.modpow(&private_exponent, &modulus).to_bytes_le()
}

pub fn derive_login_aes_material(decrypted_message: &[u8]) -> Result<[u8; 32]> {
    let mut modulus = MODULUS_BIG_ENDIAN;
    modulus.reverse();

    let mut hmac = <HmacSha256 as Mac>::new_from_slice(decrypted_message)
        .context("failed to create HMAC-SHA256")?;
    hmac.update(&modulus);

    let digest = hmac.finalize().into_bytes();
    let mut material = [0u8; 32];
    material.copy_from_slice(&digest);
    Ok(material)
}

pub fn derive_world_aes_iv(login_iv: &[u8; 16]) -> [u8; 16] {
    let digest = Sha256::digest(login_iv);
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&digest[..16]);
    iv
}

pub fn expanded_xor_transform(input: &[u8]) -> Vec<u8> {
    let needed = input.len() * 2;
    let mut xor_buffer = Vec::with_capacity(needed.max(4096));
    xor_buffer.extend_from_slice(&Sha256::digest([
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
    ]));

    while xor_buffer.len() < needed.max(4096) {
        let start = xor_buffer.len() - 16;
        let digest = Sha256::digest(&xor_buffer[start..]);
        xor_buffer.extend_from_slice(&digest);
    }

    input
        .iter()
        .enumerate()
        .map(|(index, byte)| byte ^ xor_buffer[index + input.len()])
        .collect()
}

pub struct AesCtr {
    cipher: Aes128,
    counter: [u8; 16],
    mask: [u8; 16],
    mask_pos: usize,
}

impl AesCtr {
    pub fn new(key: &[u8; 16], iv: &[u8; 16]) -> Self {
        Self {
            cipher: Aes128::new(key.into()),
            counter: *iv,
            mask: [0; 16],
            mask_pos: 16,
        }
    }

    pub fn transform(&mut self, input: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(input.len());
        for byte in input {
            if self.mask_pos == self.mask.len() {
                self.refill_mask();
            }

            output.push(byte ^ self.mask[self.mask_pos]);
            self.mask_pos += 1;
        }
        output
    }

    fn refill_mask(&mut self) {
        let mut block = self.counter.into();
        self.cipher.encrypt_block(&mut block);
        self.mask.copy_from_slice(&block);

        for byte in &mut self.counter {
            *byte = byte.wrapping_add(1);
            if *byte != 0 {
                break;
            }
        }

        self.mask_pos = 0;
    }
}
