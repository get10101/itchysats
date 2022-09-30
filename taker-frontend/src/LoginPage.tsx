import {
    Alert,
    AlertDescription,
    AlertIcon,
    Button,
    Flex,
    Heading,
    Input,
    InputGroup,
    InputRightElement,
    VStack,
} from "@chakra-ui/react";
import * as React from "react";
import { FormEvent, useEffect } from "react";
import { useState } from "react";
import useAuth from "./authentication/useAuth";
import logo from "./images/logo.svg";

export default function Login() {
    const { login, loading, error } = useAuth();
    const [show, setShow] = useState(false);
    const handleClick = () => setShow(!show);

    const [password, setPassword] = useState("weareallsatoshi");

    useEffect(() => {
        if (localStorage.getItem("oldUser") != null) {
            console.log("Not a new user, no need to prefill the default password");
            setPassword("");
        }
    }, []);

    function handleSubmit(event: FormEvent<HTMLFormElement>) {
        event.preventDefault();
        login(password);
    }

    return (
        <form onSubmit={handleSubmit}>
            <Flex justifyContent={"center"} alignItems={"center"} height={"100vh"}>
                <VStack spacing="6">
                    <Heading>Welcome Satoshi</Heading>
                    <img src={logo} className="Logo" alt="logo" />
                    <InputGroup size="md">
                        <Input
                            pr="4.5rem"
                            type={show ? "text" : "password"}
                            placeholder="Enter password"
                            value={password}
                            onChange={(e) => setPassword(e.target.value)}
                        />
                        <InputRightElement width="4.5rem">
                            <Button h="1.75rem" size="sm" onClick={handleClick}>
                                {show ? "Hide" : "Show"}
                            </Button>
                        </InputRightElement>
                    </InputGroup>

                    <Button disabled={loading} variant={"solid"} colorScheme={"blue"} isLoading={loading} type="submit">
                        Submit
                    </Button>

                    {error
                        && (
                            <Alert status="error">
                                <AlertIcon />
                                <AlertDescription>{error.detail}</AlertDescription>
                            </Alert>
                        )}
                </VStack>
            </Flex>
        </form>
    );
}
