// vm_release_disk is run inside the verified normal-release Corral module.
// Deliberately call LocalBuilder rather than corral-bootc's CLI: the latter
// fills an empty --ssh-key from the invoking runner's ~/.ssh.
package main

import (
	"fmt"
	"os"
	"regexp"

	"github.com/tuna-os/corral/pkg/bootc"
)

func main() {
	if len(os.Args) != 3 || !regexp.MustCompile(`^ghcr\.io/tuna-os/marlin@sha256:[0-9a-f]{64}$`).MatchString(os.Args[1]) {
		fmt.Fprintln(os.Stderr, "usage: vm_release_disk ghcr.io/tuna-os/marlin@sha256:DIGEST NEW.raw")
		os.Exit(2)
	}
	if _, err := os.Lstat(os.Args[2]); !os.IsNotExist(err) {
		fmt.Fprintln(os.Stderr, "destination must not exist")
		os.Exit(2)
	}
	// No SSH key, account, password, customization or provisioning script.
	// Preserve the image's real firstboot/GDM policy. Serial output is the
	// upstream Corral/TunaOS boot-gate convention, not a fixture bypass.
	_, err := (bootc.LocalBuilder{Sudo: true}).Build(bootc.BuildRequest{
		Image: os.Args[1], Dest: os.Args[2], Size: "40G",
		SSHKey: "", Kargs: []string{"console=tty0", "console=ttyS0,115200n8"},
	}, func(message string) { fmt.Fprintln(os.Stderr, message) })
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
