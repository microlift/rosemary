CC = clang
CFLAGS = -Wall -g -nostdlib -target x86_64-apple-darwin
DARWIN_LINK_FLAGS = -fuse-ld=lld -dynamiclib -Wl,-undefined,dynamic_lookup

all: example_extension

example_extension: lib.o
	$(CC) $(CFLAGS) $(DARWIN_LINK_FLAGS) -o example_extension.dylib lib.o

lib.o: lib.c
	$(CC) $(CFLAGS) -c lib.c

clean:
	rm -f *.o example_extension.dylib
