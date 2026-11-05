package crypto

import (
	"encoding/hex"
	"testing"
)

// Reference vectors produced by the former Rust implementation
// (crates/rapture_common/src/crypto.rs) for the same fixed inputs.
const (
	wantRSA      = "e7f540e6d60aa2de311bff565195c4e93f032e61b93ccd0d18e0233baa6182f0130c2a7f97f02e26abd7ba5539e9dbf8a415df813f92140ae743622b39fdba1318f2bfbfecb2eeb561c8d0868772073a206d22ffcbf5bb42cb50858159cf7637f1bcda952538b7e8ea33d16e11dd885bf531438412849d4f911c1aba54c91f67"
	wantMaterial = "f8d640b17d4c79437fbc7aa49f359b1d57380213b87f3d2326c572ecf92f462c"
	wantCTR      = "b636e2919dff89096f2854b3ac5fda9c1caffc9a58f3dc27c93392550c6fc369cc4a073b98b064dd65baecd82fc56082e14bfb18433a495aca724fe766c248ed4d60fde62669d128207b6cd719a78d81bb6e0d94bdf8a7c927b41b66d4fd26ab4b034aca41596279bd51f82b5dd8b4e3b3bb7f0b50bb499aca0cf18e23247fefe8faae86bb8a6e4f5bf65a31b3855f49847b6e35f57a387bd33a2d625514869af776a7d12fd3ae7bf00c52323924ee15cb4c29d07305b994ae9cc2c968df8df3b53af02367e602664511e0117df5a4b2f58815c36633fad0d4f95216ca971ed2afb21764302ace52efc45c67a625df75f2b44842ac66478763494be943a834da0c4fac953ada01c3456da1a41517d4a0ec4c7c119dc2146962e32cd19161e9dbffe9b4f412bc4e2bc392ab50"
	wantWorldIV  = "552604cb45e27850b7dedf748553b762"
	wantXOR      = "d5cf6b8be4b336de756faa670a3aa2155643e50c579394095e88aadec4a3d77996e878328cbad6cd"
)

func TestMatchesRustImplementation(t *testing.T) {
	encrypted := make([]byte, 128)
	for i := range encrypted {
		encrypted[i] = byte((i*7 + 3) % 251)
	}
	decrypted := DecryptLoginRSA(encrypted)
	check(t, "rsa", decrypted, wantRSA)
	material := DeriveLoginAESMaterial(decrypted)
	check(t, "material", material[:], wantMaterial)
	var key, iv [16]byte
	copy(key[:], material[:16])
	copy(iv[:], material[16:])
	data := make([]byte, 300)
	for i := range data {
		data[i] = byte(i % 256)
	}
	check(t, "ctr", NewAESCTR(key, iv).Transform(data), wantCTR)
	worldIV := DeriveWorldAESIV(iv)
	check(t, "world iv", worldIV[:], wantWorldIV)
	check(t, "xor", ExpandedXOR(data[:40]), wantXOR)
}

func TestCTRIsSymmetricAcrossCalls(t *testing.T) {
	var key, iv [16]byte
	iv[0] = 0xff
	plain := []byte("rapture ctr stream spans more than one block")
	enc := NewAESCTR(key, iv)
	cipher := append(enc.Transform(plain[:7]), enc.Transform(plain[7:])...)
	if got := string(NewAESCTR(key, iv).Transform(cipher)); got != string(plain) {
		t.Fatalf("round trip = %q", got)
	}
}

func check(t *testing.T, name string, got []byte, want string) {
	t.Helper()
	if hex.EncodeToString(got) != want {
		t.Fatalf("%s mismatch:\n got %x\nwant %s", name, got, want)
	}
}
