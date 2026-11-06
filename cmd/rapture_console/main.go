// Command rapture_console starts and stops rapture_login and rapture_world
// from a plain terminal prompt: "start server" and "shutdown server".
package main

import (
	"bufio"
	"fmt"
	"os"
	"strings"

	"github.com/Spelunkern/Rapture/internal/control"
)

func short(name string) string { return strings.TrimPrefix(name, "rapture_") }

func status() {
	state := func(name string) string {
		if control.Running(name) {
			return "ON"
		}
		return "OFF"
	}
	fmt.Printf("login: %s | world: %s\n", state("rapture_login"), state("rapture_world"))
}

func main() {
	fmt.Println("Rapture Console")
	status()
	fmt.Println(`Commands: "start server" | "shutdown server"`)
	input := bufio.NewScanner(os.Stdin)
	for {
		fmt.Print("> ")
		if !input.Scan() {
			return
		}
		switch strings.ToLower(strings.TrimSpace(input.Text())) {
		case "start server":
			for _, name := range control.Servers {
				control.Stop(name)
				if err := control.Start(name); err != nil {
					fmt.Printf("%s: error -- %v\n", short(name), err)
				} else {
					fmt.Printf("%s: started\n", short(name))
				}
			}
			status()
		case "shutdown server":
			for _, name := range control.Servers {
				control.Stop(name)
				fmt.Printf("%s: stopped\n", short(name))
			}
			status()
		case "":
		default:
			fmt.Println(`unknown command. Use "start server" or "shutdown server".`)
		}
	}
}
