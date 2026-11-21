package main

import "fmt"

func main() {
	a := make(chan int64, 1)
	b := make(chan int64, 2)
	go func() {
		for i := int64(1); i <= 2000; i++ {
			a <- i
		}
		close(a)
	}()
	go func() {
		for x := range a {
			b <- (x * x) % 1000003
		}
		close(b)
	}()
	var acc, count int64
	for y := range b {
		acc = (acc*31 + y) % 1000000007
		count++
	}
	fmt.Printf("count %d\nacc %d\n", count, acc)
}
