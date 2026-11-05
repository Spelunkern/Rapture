// Package crypto implements the Shaiya 5.4 login/world transport security:
// an RSA key exchange at login, HMAC-SHA256 key derivation, a little-endian
// counter AES-128-CTR stream and the world handshake's expanded XOR.
package crypto

import (
	"crypto/aes"
	"crypto/cipher"
	"crypto/hmac"
	"crypto/sha256"
	"math/big"
	"slices"
)

var exponent = func() [64]byte {
	var e [64]byte
	for i := range e {
		e[i] = 49
	}
	return e
}()

var privateExponentBE = [128]byte{
	8, 138, 199, 186, 85, 15, 58, 210, 200, 61, 195, 4, 184, 179, 34, 206, 136, 195, 217, 130, 183,
	109, 186, 169, 227, 164, 127, 20, 117, 176, 28, 24, 90, 198, 187, 187, 40, 5, 51, 95, 248, 64,
	57, 242, 169, 64, 223, 55, 116, 120, 193, 221, 70, 197, 63, 248, 58, 55, 238, 18, 208, 78, 2,
	45, 37, 179, 75, 13, 162, 144, 149, 59, 224, 204, 34, 103, 167, 209, 57, 110, 0, 49, 210, 152,
	80, 163, 80, 12, 168, 99, 197, 68, 0, 242, 57, 14, 94, 9, 44, 39, 251, 171, 79, 15, 227, 187,
	186, 11, 197, 38, 102, 237, 225, 95, 163, 103, 47, 182, 138, 183, 143, 151, 42, 251, 168, 233,
	0, 103,
}

var modulusBE = [128]byte{
	149, 236, 7, 136, 170, 27, 149, 37, 73, 12, 247, 7, 100, 209, 106, 211, 222, 97, 144, 12, 143,
	217, 169, 171, 61, 24, 247, 7, 180, 99, 104, 179, 227, 100, 28, 163, 170, 190, 85, 11, 6, 120,
	254, 97, 221, 101, 188, 248, 39, 66, 9, 148, 75, 146, 151, 125, 69, 18, 217, 2, 45, 11, 228,
	27, 197, 93, 207, 127, 153, 22, 227, 219, 19, 30, 152, 203, 144, 16, 185, 118, 237, 46, 122,
	155, 231, 221, 197, 7, 154, 240, 94, 207, 90, 123, 62, 67, 170, 125, 25, 71, 203, 254, 226,
	181, 2, 215, 94, 248, 172, 203, 189, 98, 164, 48, 193, 67, 93, 248, 138, 163, 249, 113, 186,
	61, 76, 25, 170, 229,
}

func reversed(b []byte) []byte {
	out := slices.Clone(b)
	slices.Reverse(out)
	return out
}

// LoginPublicKey returns the exponent and the little-endian modulus sent in
// the 0xA101 handshake.
func LoginPublicKey() (exp [64]byte, modulusLE [128]byte) {
	copy(modulusLE[:], reversed(modulusBE[:]))
	return exponent, modulusLE
}

func leInt(b []byte) *big.Int { return new(big.Int).SetBytes(reversed(b)) }

func leBytes(v *big.Int) []byte {
	out := reversed(v.Bytes())
	if len(out) == 0 {
		out = []byte{0}
	}
	return out
}

// DecryptLoginRSA decrypts the client's little-endian RSA block.
func DecryptLoginRSA(encrypted []byte) []byte {
	c := leInt(encrypted)
	d := new(big.Int).SetBytes(privateExponentBE[:])
	n := new(big.Int).SetBytes(modulusBE[:])
	return leBytes(new(big.Int).Exp(c, d, n))
}

// DeriveLoginAESMaterial is HMAC-SHA256(key=decrypted, msg=modulus LE):
// the first 16 bytes are the AES key, the last 16 the IV.
func DeriveLoginAESMaterial(decrypted []byte) [32]byte {
	mac := hmac.New(sha256.New, decrypted)
	mac.Write(reversed(modulusBE[:]))
	var out [32]byte
	copy(out[:], mac.Sum(nil))
	return out
}

// DeriveWorldAESIV is SHA-256(login IV)[:16].
func DeriveWorldAESIV(loginIV [16]byte) [16]byte {
	digest := sha256.Sum256(loginIV[:])
	var iv [16]byte
	copy(iv[:], digest[:16])
	return iv
}

// ExpandedXOR applies the world handshake's SHA-256 chained XOR stream.
func ExpandedXOR(input []byte) []byte {
	needed := max(len(input)*2, 4096)
	seed := sha256.Sum256([]byte{1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16})
	buf := make([]byte, 0, needed+32)
	buf = append(buf, seed[:]...)
	for len(buf) < needed {
		digest := sha256.Sum256(buf[len(buf)-16:])
		buf = append(buf, digest[:]...)
	}
	out := make([]byte, len(input))
	for i, b := range input {
		out[i] = b ^ buf[i+len(input)]
	}
	return out
}

// AESCTR is AES-128 in counter mode with a little-endian counter that
// starts at byte 0 (Go's cipher.NewCTR increments big-endian).
type AESCTR struct {
	block   cipher.Block
	counter [16]byte
	mask    [16]byte
	pos     int
}

func NewAESCTR(key, iv [16]byte) *AESCTR {
	block, err := aes.NewCipher(key[:])
	if err != nil {
		panic(err) // a 16-byte key cannot fail
	}
	return &AESCTR{block: block, counter: iv, pos: 16}
}

// Transform encrypts or decrypts input (the operation is symmetric).
func (c *AESCTR) Transform(input []byte) []byte {
	out := make([]byte, len(input))
	for i, b := range input {
		if c.pos == 16 {
			c.block.Encrypt(c.mask[:], c.counter[:])
			for j := range c.counter {
				c.counter[j]++
				if c.counter[j] != 0 {
					break
				}
			}
			c.pos = 0
		}
		out[i] = b ^ c.mask[c.pos]
		c.pos++
	}
	return out
}
