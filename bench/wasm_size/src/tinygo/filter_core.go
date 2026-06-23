// wasm_size bench — the Iris filter core in Go, built with TinyGo.
//
// A faithful port of ../kara/filter_core.kara (itself a verbatim port of
// examples/iris/src/filters.kara). Integer math throughout, so the FNV-1a
// checksums printed here MUST byte-match the Kāra and Rust ports — the
// cross-language correctness check behind the honest module-size comparison.
package main

import (
	"fmt"
	"math"
)

func imgWidth() int64  { return 512 }
func imgHeight() int64 { return 384 }

func fBlur() int64      { return 1 }
func fSharpen() int64   { return 2 }
func fEdge() int64      { return 3 }
func fInvert() int64    { return 4 }
func fGrayscale() int64 { return 5 }
func filterCount() int64 { return 6 }

func clampi(v, lo, hi int64) int64 {
	if v < lo {
		return lo
	}
	if v > hi {
		return hi
	}
	return v
}

func toByte(v int64) uint8 {
	return uint8(clampi(v, 0, 255))
}

func sourceChannel(x0, y0, ch int64) int64 {
	w := imgWidth()
	h := imgHeight()
	x := clampi(x0, 0, w-1)
	y := clampi(y0, 0, h-1)
	cx := w / 2
	cy := h / 2
	radius := h / 4

	r := (x * 255) / w
	g := (y * 255) / h
	b := ((x + y) * 255) / (w + h)

	dx := x - cx
	dy := y - cy
	if dx*dx+dy*dy < radius*radius {
		r, g, b = 250, 230, 40
	}

	if x < cx && y < cy && ((x/24)+(y/24))%2 == 0 {
		r, g, b = 20, 20, 30
	}

	if x >= cx && y >= cy && ((x+y)/16)%2 == 0 {
		r, g, b = 230, 60, 90
	}

	if ch == 0 {
		return clampi(r, 0, 255)
	}
	if ch == 1 {
		return clampi(g, 0, 255)
	}
	return clampi(b, 0, 255)
}

func luma(x, y int64) int64 {
	r := sourceChannel(x, y, 0)
	g := sourceChannel(x, y, 1)
	b := sourceChannel(x, y, 2)
	return (r*77 + g*150 + b*29) / 256
}

func blurChannel(x, y, ch int64) int64 {
	acc := int64(0)
	for dy := int64(-1); dy <= 1; dy++ {
		for dx := int64(-1); dx <= 1; dx++ {
			acc += sourceChannel(x+dx, y+dy, ch)
		}
	}
	return acc / 9
}

func sharpenChannel(x, y, ch int64) int64 {
	c := sourceChannel(x, y, ch) * 5
	n := sourceChannel(x, y-1, ch)
	s := sourceChannel(x, y+1, ch)
	e := sourceChannel(x+1, y, ch)
	west := sourceChannel(x-1, y, ch)
	return c - n - s - e - west
}

func sobel(x, y int64) int64 {
	tl := luma(x-1, y-1)
	tc := luma(x, y-1)
	tr := luma(x+1, y-1)
	ml := luma(x-1, y)
	mr := luma(x+1, y)
	bl := luma(x-1, y+1)
	bc := luma(x, y+1)
	br := luma(x+1, y+1)
	gx := (tr + 2*mr + br) - (tl + 2*ml + bl)
	gy := (bl + 2*bc + br) - (tl + 2*tc + tr)
	mag2 := float64(gx*gx + gy*gy)
	return int64(math.Sqrt(mag2))
}

func renderBand(y0, y1, filterID int64) []uint8 {
	w := imgWidth()
	out := make([]uint8, 0)
	for y := y0; y < y1; y++ {
		for x := int64(0); x < w; x++ {
			if filterID == fBlur() {
				out = append(out, toByte(blurChannel(x, y, 0)))
				out = append(out, toByte(blurChannel(x, y, 1)))
				out = append(out, toByte(blurChannel(x, y, 2)))
			} else if filterID == fSharpen() {
				out = append(out, toByte(sharpenChannel(x, y, 0)))
				out = append(out, toByte(sharpenChannel(x, y, 1)))
				out = append(out, toByte(sharpenChannel(x, y, 2)))
			} else if filterID == fEdge() {
				m := toByte(sobel(x, y))
				out = append(out, m, m, m)
			} else if filterID == fInvert() {
				out = append(out, toByte(255-sourceChannel(x, y, 0)))
				out = append(out, toByte(255-sourceChannel(x, y, 1)))
				out = append(out, toByte(255-sourceChannel(x, y, 2)))
			} else if filterID == fGrayscale() {
				l := toByte(luma(x, y))
				out = append(out, l, l, l)
			} else {
				out = append(out, toByte(sourceChannel(x, y, 0)))
				out = append(out, toByte(sourceChannel(x, y, 1)))
				out = append(out, toByte(sourceChannel(x, y, 2)))
			}
			out = append(out, 255)
		}
	}
	return out
}

func applyFull(filterID int64) []uint8 {
	return renderBand(0, imgHeight(), filterID)
}

// FNV-1a folded to 32 bits — uint32 wrapping mul matches the Kāra port's
// `wrapping_mul(16777619) % 4294967296`.
func checksum(buf []uint8) uint32 {
	var h uint32 = 2166136261
	for _, b := range buf {
		h = (h ^ uint32(b)) * 16777619
	}
	return h
}

func main() {
	for id := int64(0); id < filterCount(); id++ {
		out := applyFull(id)
		c := checksum(out)
		fmt.Printf("filter %d checksum %d\n", id, c)
	}
}
