# Makefile for out-of-tree Kbuild compilation (Android 12 Linux 5.10 GKI)
KDIR ?= $(PWD)/../../android12-5.10/out/android12-5.10/common

default:
	$(MAKE) -C $(KDIR) M=$(PWD) modules

clean:
	$(MAKE) -C $(KDIR) M=$(PWD) clean
