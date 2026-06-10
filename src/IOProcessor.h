#ifndef TETRA_IOPROCESSOR_H
#define TETRA_IOPROCESSOR_H

#include "datatypes.h"

class IOProcessor {
public:
    IOProcessor(const std::string& INPUT_FILE, const std::string& OUTPUT_FILE);
    virtual ~IOProcessor();



};


#endif // TETRA_IOPROCESSOR_H
