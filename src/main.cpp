#include <ryml/rapidyaml-0.15.2.hpp>

#include <cstring>
#include <iostream>
#include "userspace.h"

int main(const int ARGC, char *argv[]) {
    if (ARGC != 3) {
        if (ARGC != 1 && (strcmp(argv[1], "-h") != 0 || strcmp(argv[1], "--help") != 0 || strcmp(argv[1], "-?")) != 0) {
            std::cerr << "You have entered an invalid number of arguments. Please try again, or see the help menu (--help)." << std::endl;
            return 0;
        } else {
            help(argv[0]);
        }
    }

    return 0;
}
