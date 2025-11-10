package main

import "fmt"

func main() {
    fmt.Println("Hello, World!")
    greet("Go")
}

func greet(name string) {
    fmt.Printf("Hello, %s!\n", name)
}
