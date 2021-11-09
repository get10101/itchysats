import { Box, BoxProps } from "@chakra-ui/layout";
import { Center, Image, Text, VStack } from "@chakra-ui/react";
import { motion } from "framer-motion";
import React from "react";
import { useHistory } from "react-router-dom";
import logo from "../images/logo.svg";

const MotionBox = motion<BoxProps>(Box);

export const Logo = () => {
    const history = useHistory();

    function handleClick() {
        history.push("/trade");
    }

    return (
        <Center>
            <VStack>
                <MotionBox
                    height="200px"
                    width="200px"
                    whileHover={{ scale: 1.2 }}
                    whileTap={{ scale: 0.9 }}
                    animate={{
                        scale: [1, 1.5, 1.5, 1, 1],
                        rotate: [0, 360],
                    }}
                    onClick={handleClick}
                    // @ts-ignore: lint is complaining but should be fine :)
                    transition={{
                        repeat: 999,
                        repeatType: "loop",
                        duration: 2,
                    }}
                >
                    <Image src={logo} width="200px" />
                </MotionBox>

                <Text>Click to enter</Text>
            </VStack>
        </Center>
    );
};
