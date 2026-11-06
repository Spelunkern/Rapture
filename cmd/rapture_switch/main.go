// Command rapture_switch is a one-button window that starts or stops
// rapture_login and rapture_world. The servers run detached: closing the
// window leaves them as they are.
package main

import (
	_ "embed"
	"time"

	"fyne.io/fyne/v2"
	"fyne.io/fyne/v2/app"
	"fyne.io/fyne/v2/container"
	"fyne.io/fyne/v2/layout"
	"fyne.io/fyne/v2/widget"

	"github.com/Spelunkern/Rapture/internal/control"
)

//go:embed icon.png
var iconPNG []byte

const pollInterval = 2 * time.Second

func main() {
	a := app.NewWithID("com.spelunkern.rapture.switch")
	icon := fyne.NewStaticResource("icon.png", iconPNG)
	a.SetIcon(icon)
	w := a.NewWindow("Rapture")
	w.SetIcon(icon)

	status := widget.NewLabel("")
	status.Alignment = fyne.TextAlignCenter
	message := widget.NewLabel("")
	message.Alignment = fyne.TextAlignCenter
	message.Wrapping = fyne.TextWrapWord
	button := widget.NewButton("", nil)
	button.Importance = widget.HighImportance

	busy := false
	refresh := func() {
		state := func(name string) string {
			if control.Running(name) {
				return "ON"
			}
			return "OFF"
		}
		login, world := state("rapture_login"), state("rapture_world")
		running := control.AnyRunning()
		fyne.Do(func() {
			status.SetText("login: " + login + "   |   world: " + world)
			if busy {
				return
			}
			if running {
				button.SetText("Stop server")
				button.Importance = widget.DangerImportance
			} else {
				button.SetText("Start server")
				button.Importance = widget.HighImportance
			}
			button.Enable()
			button.Refresh()
		})
	}

	button.OnTapped = func() {
		busy = true
		button.Disable()
		stopping := control.AnyRunning()
		if stopping {
			button.SetText("Stopping...")
		} else {
			button.SetText("Starting...")
		}
		go func() {
			var err error
			if stopping {
				control.StopAll()
			} else {
				err = control.StartAll()
				time.Sleep(time.Second) // let a failing server exit before the status check
			}
			fyne.Do(func() {
				busy = false
				switch {
				case err != nil:
					message.SetText(err.Error())
				case stopping:
					message.SetText("Server stopped.")
				default:
					message.SetText("Server started.")
				}
			})
			refresh()
		}()
	}

	go func() {
		for {
			refresh()
			time.Sleep(pollInterval)
		}
	}()

	w.SetContent(container.NewPadded(container.NewVBox(
		status,
		container.NewHBox(layout.NewSpacer(), container.NewGridWrap(fyne.NewSize(220, 48), button), layout.NewSpacer()),
		message,
	)))
	w.Resize(fyne.NewSize(340, 170))
	w.SetFixedSize(true)
	w.ShowAndRun()
}
